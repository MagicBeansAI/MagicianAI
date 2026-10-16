//! Durable canonical records for the app-platform control and data planes.
//!
//! These records describe state; they do not grant authority. Future stores and
//! dispatch adapters must resolve authenticated scope and current revisions
//! independently before acting on any deserialized value.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use magician_app_contract::contribution::{
    AppContributionEvidenceClass, AppMemorySemanticDestinationV1, AppMemoryTierScopeV1,
    APP_CONTRIBUTION_MAX_AUDIENCES, APP_CONTRIBUTION_MAX_SELECTED_FIELDS,
    APP_CONTRIBUTION_MAX_TTL_MS,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    interactive::AppReviewedInteractiveCapabilityGrant,
    lifecycle::{AppInstallationLifecycle, AppInstallationStatus, AppLifecycleAttemptState},
    manifest::{
        event_behavior_projection_schema, AppManifestNavigationEntry,
        APP_BEHAVIOR_MAX_CAUSATION_DEPTH, APP_BEHAVIOR_MAX_CONTRIBUTION_PROPOSALS_PER_RUN,
        APP_BEHAVIOR_MAX_INTERVAL_SECONDS, APP_BEHAVIOR_MAX_OPERATIONS,
        APP_BEHAVIOR_MAX_PERIOD_SECONDS, APP_BEHAVIOR_MAX_PURPOSE_BYTES,
        APP_BEHAVIOR_MAX_SPEND_DEPTH, APP_BEHAVIOR_MAX_STARTS_PER_PERIOD,
        APP_BEHAVIOR_MIN_INTERVAL_SECONDS, APP_CUSTOM_SURFACE_MAX_ENTRY_POINTS,
        APP_EVENT_BEHAVIOR_MAX_DECLARATIONS, APP_NAVIGATION_MAX_ENTRIES,
        APP_NOTIFICATION_MAX_PENDING, APP_NOTIFICATION_MAX_PERIOD_SECONDS,
        APP_NOTIFICATION_MAX_PER_PERIOD, APP_NOTIFICATION_MAX_PURPOSE_BYTES,
        APP_NOTIFICATION_MAX_TOTAL_PORTS, APP_NOTIFICATION_MAX_TTL_SECONDS,
        APP_NOTIFICATION_MIN_PERIOD_SECONDS, APP_NOTIFICATION_MIN_TTL_SECONDS,
        APP_SURFACING_MAX_TITLE_BYTES,
    },
    models::{
        validate_bounded, validate_json_value, validate_nonempty_bounded, AppContractError,
        AppContractLimits, AppDataClassification, AppDigest, AppFieldPath, AppInstallationId,
        AppModelProcessing, AppName, AppRecordId, AppReference, AppRevision, AppScopeBindingRef,
        ValidateAppContract,
    },
};

pub const APP_CONTRIBUTION_MAX_PORTS_PER_WORKFLOW: usize = 32;
pub const APP_CONTRIBUTION_MAX_PURPOSES: usize = 16;
pub const APP_CONTRIBUTION_MAX_EVIDENCE_CLASSES: usize = 3;
pub const APP_CONTRIBUTION_MAX_PROPOSALS_PER_WINDOW: u16 = 10_000;
pub const APP_CONTRIBUTION_MAX_FREQUENCY_WINDOW_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const APP_CONTRIBUTION_MEMORY_USER_AUDIENCE: &str = "user:owner";
pub const APP_CONTRIBUTION_PERSONAL_ASSISTANT_AUDIENCE: &str = "agent:personal-assistant";

/// V1 admits only the exact entity projection backed by the declaring
/// workflow's mutation receipt. There is intentionally no standalone typed
/// value, arbitrary record query, prompt text, artifact, or app-supplied
/// source-reference variant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppContributionSource {
    MutationBackedEntityProjection {
        entity: AppName,
        selected_fields: Vec<AppFieldPath>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppContributionDestination {
    Memory,
    PersonalAgentRetrieval,
}

/// Closed destination-body choices supported by V1. These variants map
/// one-to-one to destination DTOs: no task can reinterpret a generic port as a
/// different tier, semantic bucket, agent, or goal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppContributionDestinationBinding {
    MemoryUserKnowledge,
    PersonalAssistantRetrievalNoGoal,
}

impl AppContributionDestinationBinding {
    pub fn for_destination(destination: AppContributionDestination) -> Self {
        match destination {
            AppContributionDestination::Memory => Self::MemoryUserKnowledge,
            AppContributionDestination::PersonalAgentRetrieval => {
                Self::PersonalAssistantRetrievalNoGoal
            },
        }
    }

    pub fn destination(self) -> AppContributionDestination {
        match self {
            Self::MemoryUserKnowledge => AppContributionDestination::Memory,
            Self::PersonalAssistantRetrievalNoGoal => {
                AppContributionDestination::PersonalAgentRetrieval
            },
        }
    }

    pub fn required_audience(self) -> &'static str {
        match self {
            Self::MemoryUserKnowledge => APP_CONTRIBUTION_MEMORY_USER_AUDIENCE,
            Self::PersonalAssistantRetrievalNoGoal => APP_CONTRIBUTION_PERSONAL_ASSISTANT_AUDIENCE,
        }
    }

    pub fn memory_body(self) -> Option<(AppMemoryTierScopeV1, AppMemorySemanticDestinationV1)> {
        (self == Self::MemoryUserKnowledge).then_some((
            AppMemoryTierScopeV1::User,
            AppMemorySemanticDestinationV1::Knowledge,
        ))
    }

    pub fn retrieval_target(self) -> Option<(&'static str, Option<&'static str>)> {
        (self == Self::PersonalAssistantRetrievalNoGoal).then_some(("personal-assistant", None))
    }
}

/// A rate ceiling, not a scheduler. A narrower grant permits fewer proposals
/// over an equal-or-longer window.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct AppContributionFrequency {
    pub max_proposals: u16,
    pub window_seconds: u64,
}

impl AppContributionFrequency {
    pub fn validate(self) -> Result<(), AppContractError> {
        if self.max_proposals == 0
            || self.max_proposals > APP_CONTRIBUTION_MAX_PROPOSALS_PER_WINDOW
            || self.window_seconds == 0
            || self.window_seconds > APP_CONTRIBUTION_MAX_FREQUENCY_WINDOW_SECONDS
        {
            return Err(AppContractError::invalid(
                "contribution_frequency",
                "contains a zero or unsupported proposal/window ceiling",
            ));
        }
        Ok(())
    }

    pub fn narrows(self, requested: Self) -> bool {
        self.max_proposals <= requested.max_proposals
            && self.window_seconds >= requested.window_seconds
    }
}

/// Exact owner-approved per-workflow port ceiling. It is durable review
/// evidence, not standalone authority: dispatch must re-bind it to the current
/// grant, consumed approval, immutable package lock, and task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReviewedContributionPortGrant {
    pub schema: String,
    pub workflow_id: AppName,
    pub port_id: AppName,
    pub locked_port_digest: AppDigest,
    pub source: AppContributionSource,
    pub destination: AppContributionDestination,
    pub destination_binding: AppContributionDestinationBinding,
    pub purposes: Vec<AppName>,
    pub audiences: Vec<AppReference>,
    pub evidence_classes: Vec<AppContributionEvidenceClass>,
    pub frequency: AppContributionFrequency,
    pub maximum_retention_seconds: u64,
    pub grant_digest: AppDigest,
}

impl AppReviewedContributionPortGrant {
    pub fn seal(mut self) -> Result<Self, AppContractError> {
        self.grant_digest = reviewed_contribution_port_grant_digest(&self)?;
        self.validate()?;
        Ok(self)
    }

    pub(crate) fn validate(&self) -> Result<(), AppContractError> {
        if self.schema != "magician.app-reviewed-contribution-port-grant.v1" {
            return Err(AppContractError::invalid(
                "contribution_grant.schema",
                "is not a supported contribution-grant schema",
            ));
        }
        self.frequency.validate()?;
        if self.destination_binding.destination() != self.destination
            || self.audiences.len() != 1
            || self.audiences[0].as_str() != self.destination_binding.required_audience()
        {
            return Err(AppContractError::invalid(
                "contribution_grant.destination_binding",
                "does not match the fixed V1 destination body and audience",
            ));
        }
        let ttl_ms = i64::try_from(self.maximum_retention_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000));
        if self.maximum_retention_seconds == 0
            || ttl_ms.is_none()
            || ttl_ms.is_some_and(|ttl_ms| ttl_ms > APP_CONTRIBUTION_MAX_TTL_MS)
        {
            return Err(AppContractError::invalid(
                "contribution_grant.maximum_retention_seconds",
                "is zero or exceeds the destination contract TTL ceiling",
            ));
        }
        let AppContributionSource::MutationBackedEntityProjection {
            selected_fields, ..
        } = &self.source;
        validate_nonempty_unique_contract_values(
            "contribution_grant.selected_fields",
            selected_fields,
            APP_CONTRIBUTION_MAX_SELECTED_FIELDS,
        )?;
        validate_nonempty_unique_contract_values(
            "contribution_grant.purposes",
            &self.purposes,
            APP_CONTRIBUTION_MAX_PURPOSES,
        )?;
        if self.purposes.len() != 1 {
            return Err(AppContractError::invalid(
                "contribution_grant.purposes",
                "V1 requires exactly one destination purpose",
            ));
        }
        validate_nonempty_unique_contract_values(
            "contribution_grant.audiences",
            &self.audiences,
            APP_CONTRIBUTION_MAX_AUDIENCES,
        )?;
        validate_nonempty_unique_contract_values(
            "contribution_grant.evidence_classes",
            &self.evidence_classes,
            APP_CONTRIBUTION_MAX_EVIDENCE_CLASSES,
        )?;
        if self.evidence_classes.len() != 1
            || self.evidence_classes[0] == AppContributionEvidenceClass::Authoritative
        {
            return Err(AppContractError::invalid(
                "contribution_grant.evidence_classes",
                "V1 requires one derived or hypothesis evidence class",
            ));
        }
        if reviewed_contribution_port_grant_digest(self)? != self.grant_digest {
            return Err(AppContractError::invalid(
                "contribution_grant.grant_digest",
                "does not match the reviewed contribution ceiling",
            ));
        }
        Ok(())
    }
}

fn validate_nonempty_unique_contract_values<T: Eq + std::hash::Hash>(
    field: &'static str,
    values: &[T],
    maximum: usize,
) -> Result<(), AppContractError> {
    if values.is_empty() || values.len() > maximum {
        return Err(AppContractError::invalid(
            field,
            "is empty or exceeds its collection ceiling",
        ));
    }
    let mut unique = HashSet::with_capacity(values.len());
    if values.iter().any(|value| !unique.insert(value)) {
        return Err(AppContractError::invalid(field, "contains a duplicate"));
    }
    Ok(())
}

fn reviewed_contribution_port_grant_digest(
    grant: &AppReviewedContributionPortGrant,
) -> Result<AppDigest, AppContractError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "schema": grant.schema,
        "workflow_id": grant.workflow_id,
        "port_id": grant.port_id,
        "locked_port_digest": grant.locked_port_digest,
        "source": grant.source,
        "destination": grant.destination,
        "destination_binding": grant.destination_binding,
        "purposes": grant.purposes,
        "audiences": grant.audiences,
        "evidence_classes": grant.evidence_classes,
        "frequency": grant.frequency,
        "maximum_retention_seconds": grant.maximum_retention_seconds,
    }))
    .map_err(|error| AppContractError::invalid("contribution_grant", error.to_string()))
}

pub(crate) fn validate_reviewed_contribution_port_grants(
    grants: &[AppReviewedContributionPortGrant],
    maximum: usize,
) -> Result<(), AppContractError> {
    if grants.len() > maximum {
        return Err(AppContractError::invalid(
            "contribution_grants",
            "exceeds the collection ceiling",
        ));
    }
    let mut previous: Option<(&AppName, &AppName)> = None;
    for grant in grants {
        grant.validate()?;
        let key = (&grant.workflow_id, &grant.port_id);
        if previous.is_some_and(|prior| prior >= key) {
            return Err(AppContractError::invalid(
                "contribution_grants",
                "is duplicated or not in canonical workflow/port order",
            ));
        }
        previous = Some(key);
    }
    Ok(())
}

/// Canonical installed authority identity. Contribution grants live on the
/// consumed approval, while interactive grants live on both the grant and its
/// approval projection. Including both axes here makes omission, narrowing or
/// substitution change the exact authority digest used by launch and terminal
/// settlement. Granted custom-surface entry points (plan 1.6) join the same
/// rule, but the axis is folded in ONLY when non-empty: grants that predate
/// scripted surfaces must keep byte-identical digests, because consumers
/// (launch settlement, contribution binding) recompute this digest and
/// compare it against the value persisted at approval.
pub fn app_granted_authority_digest(
    grant: &AppGrantRevision,
    contribution_grants: &[AppReviewedContributionPortGrant],
) -> Result<AppDigest, AppContractError> {
    validate_reviewed_contribution_port_grants(
        contribution_grants,
        AppContractLimits::default().max_collection_items(),
    )?;
    let legacy_axes = serde_json::json!({
        "granted_tools": grant.granted_tools,
        "granted_agents": grant.granted_agents,
        "granted_personalities": grant.granted_personalities,
        "granted_context_reads": grant.granted_context_reads,
        "granted_personal_agent_data_access": grant.granted_personal_agent_data_access,
        "granted_data_handling_policy": grant.granted_data_handling_policy,
        "granted_background_execution": grant.granted_background_execution,
        "granted_network_policy": grant.granted_network_policy,
        "granted_resource_ceiling": grant.granted_resource_ceiling,
    });
    if contribution_grants.is_empty()
        && grant.granted_interactive_capabilities.is_empty()
        && grant.granted_custom_surface_entry_points.is_empty()
        && grant.granted_behavior_grants.is_empty()
        && grant.granted_event_behavior_grants.is_empty()
        && grant.granted_notification_grants.is_empty()
        && grant.granted_memory_read.is_none()
        && grant.granted_secret_uses.is_none()
        && !grant.granted_any_public_host
    {
        return AppDigest::blake3_canonical_json(&legacy_axes)
            .map_err(|error| AppContractError::invalid("grant_authority", error.to_string()));
    }
    let mut axes = serde_json::json!({
        "authority_axes": legacy_axes,
        "contribution_grants": contribution_grants,
        "interactive_capability_grants": grant.granted_interactive_capabilities,
    });
    if !grant.granted_custom_surface_entry_points.is_empty() {
        axes["custom_surface_entry_point_grants"] =
            serde_json::to_value(&grant.granted_custom_surface_entry_points)
                .map_err(|error| AppContractError::invalid("grant_authority", error.to_string()))?;
    }
    if !grant.granted_behavior_grants.is_empty() {
        axes["behavior_grants"] = serde_json::to_value(&grant.granted_behavior_grants)
            .map_err(|error| AppContractError::invalid("grant_authority", error.to_string()))?;
    }
    if !grant.granted_event_behavior_grants.is_empty() {
        axes["event_behavior_grants"] = serde_json::to_value(&grant.granted_event_behavior_grants)
            .map_err(|error| AppContractError::invalid("grant_authority", error.to_string()))?;
    }
    if !grant.granted_notification_grants.is_empty() {
        axes["notification_grants"] = serde_json::to_value(&grant.granted_notification_grants)
            .map_err(|error| AppContractError::invalid("grant_authority", error.to_string()))?;
    }
    if let Some(memory_read) = &grant.granted_memory_read {
        axes["memory_read_grant"] = serde_json::to_value(memory_read)
            .map_err(|error| AppContractError::invalid("grant_authority", error.to_string()))?;
    }
    if let Some(secret_uses) = &grant.granted_secret_uses {
        axes["secret_use_grants"] = serde_json::to_value(secret_uses)
            .map_err(|error| AppContractError::invalid("grant_authority", error.to_string()))?;
    }
    // Only a grant that carries it: every other install keeps its digest.
    if grant.granted_any_public_host {
        axes["any_public_host_grant"] = serde_json::Value::Bool(true);
    }
    AppDigest::blake3_canonical_json(&axes)
        .map_err(|error| AppContractError::invalid("grant_authority", error.to_string()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct AppScope {
    pub principal: AppReference,
    pub workspace: AppReference,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppPackageSourceKind {
    Bundled,
    LocalAuthoring,
    LocalVibedev,
    Marketplace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCompatibilityRequirement {
    pub contract: AppName,
    pub requirement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPackageRevision {
    pub package_id: AppReference,
    pub semantic_version: String,
    pub content_digest: AppDigest,
    pub manifest_schema_version: String,
    pub authoring_sdk_version: String,
    pub publisher_identity: AppReference,
    pub source_kind: AppPackageSourceKind,
    pub compatibility: Vec<AppCompatibilityRequirement>,
    pub requested_authority_digest: AppDigest,
    pub requested_data_policy_digest: AppDigest,
    pub dependency_lock_digest: AppDigest,
    pub entity_schema_digest: AppDigest,
    pub view_schema_digest: AppDigest,
    pub workflow_digest: AppDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_attestation_ref: Option<AppReference>,
    pub conformance_attestation_ref: AppReference,
    pub created_at: DateTime<Utc>,
}

/// Non-sensitive package metadata used by the scoped Apps directory. This is
/// persisted beside the immutable package revision at admission time so
/// directory reads never re-open/re-admit a potentially large bundle and never
/// inspect app entity records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPackageDirectoryMetadata {
    pub package_revision_ref: AppReference,
    pub name: AppName,
    pub description: String,
    pub actions: Vec<AppName>,
    /// Custom-surface entry points declared by the admitted manifest's
    /// `app.custom_surface.entry_points` (plan 1.6), captured here so the
    /// scoped directory can advertise custom-surface hostability without
    /// re-opening the bundle. `default` + `skip_serializing_if` keep
    /// pre-existing persisted metadata decoding unchanged and its canonical
    /// bytes identical.
    #[serde(default, skip_serializing_if = "is_zero_custom_surface_entry_count")]
    pub custom_surface_entry_count: usize,
    /// First-party navigation declared by the admitted manifest's
    /// `app.navigation` (gate S3), captured here for the same reason as the
    /// custom-surface count: the scoped directory must be able to project it
    /// without re-opening the bundle.
    ///
    /// This vector cannot be non-empty for a package an owner or the
    /// marketplace published. `validate_widget_declarations` refuses
    /// navigation on any manifest that is not system-distribution, and both
    /// the ordinary staging path and untrusted candidate publication refuse a
    /// system-distribution manifest outright — only host-controlled,
    /// digest-pinned boot admission admits one. Capturing the declaration here
    /// therefore carries that fence forward rather than widening it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub navigation: Vec<AppManifestNavigationEntry>,
    pub manifest_digest: AppDigest,
    pub created_at: DateTime<Utc>,
}

fn is_zero_custom_surface_entry_count(value: &usize) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInstallation {
    pub scope: AppScope,
    pub installation_id: AppInstallationId,
    pub package_revision_ref: AppReference,
    pub lifecycle: AppInstallationLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_revision: Option<AppRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_schema_revision: Option<AppRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_surface_revision: Option<AppRevision>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantined_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uninstalled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppLifecycleAttemptKind {
    InitialInstall,
    Reinstall,
    Update,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLifecycleAttempt {
    pub attempt_id: AppReference,
    pub kind: AppLifecycleAttemptKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<AppInstallationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_installation_generation: Option<u64>,
    pub candidate_package_revision_ref: AppReference,
    pub state: AppLifecycleAttemptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conformance_attestation_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_migration_diff_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<AppName>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppPersonalAgentAccess {
    Denied,
    ApprovedProjection,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppMemoryPromotion {
    Denied,
    CandidateAllowed,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppExternalEgress {
    Denied,
    ApprovedDestinations,
    /// `app_in_place_skill_v1`: the app asks that its OS-jail tools may reach
    /// any public HTTPS host (plus any approved destinations). Ordered above
    /// `ApprovedDestinations`, so every ceiling intersects it away by `min`;
    /// it takes effect only with the owner's explicit
    /// `granted_any_public_host`.
    AnyPublicHost,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDataHandlingPolicy {
    pub classification_floor: AppDataClassification,
    pub model_processing: AppModelProcessing,
    pub personal_agent_access: AppPersonalAgentAccess,
    pub memory_promotion: AppMemoryPromotion,
    pub external_egress: AppExternalEgress,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_destinations: Vec<AppReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEntityProjectionGrant {
    pub entity: AppName,
    pub fields: Vec<AppFieldPath>,
    #[serde(default)]
    pub search: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppBackgroundExecution {
    Denied,
    Granted {
        min_interval_seconds: u64,
        max_concurrent_runs: u16,
    },
}

/// Owner-reviewable and runtime-enforceable resource ceiling for one behavior.
/// Monetary values use micro-USD, matching the canonical app resource ledger.
#[derive(
    Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(deny_unknown_fields)]
pub struct AppBehaviorResourceCeiling {
    pub max_tokens_per_run: u64,
    pub max_cost_microusd_per_run: u64,
    pub max_active_seconds_per_run: u64,
    pub max_tokens_per_month: u64,
    pub max_cost_microusd_per_month: u64,
    pub max_starts_per_period: u32,
    pub period_seconds: u64,
    pub max_causation_depth: u16,
    pub max_spend_depth: u16,
    pub max_contribution_proposals_per_run: u16,
}

/// Exact requested or owner-narrowed authority for one scheduled behavior.
/// The reviewed digest binds the manifest behavior plus its requested resource
/// ceiling. A granted entry may increase the minimum cadence or lower numeric
/// ceilings, but may not substitute its purpose, action, operations, schema,
/// or digest.
/// The operation allow-set may be empty for a deterministic behavior, in which
/// case no model-output schema digest is valid.
#[derive(
    Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(deny_unknown_fields)]
pub struct AppBehaviorGrant {
    pub behavior_id: AppName,
    /// Exact owner-reviewed purpose used by the behavior dispatcher. Keeping
    /// the text on the grant makes it visible in installation review and
    /// prevents a package update from substituting model instructions behind
    /// an otherwise unchanged selector/action/operation tuple.
    pub purpose: String,
    pub action: AppName,
    pub input_selector_digest: AppDigest,
    pub operations: Vec<AppName>,
    /// Digest of the ordered operation recipe. `operations` alone is an
    /// allow-set: without this a package update could reorder the steps, drop
    /// a guard, or make a guarded step unconditional while the reviewed
    /// selector/action/operation tuple stayed byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema_digest: Option<AppDigest>,
    pub min_interval_seconds: u64,
    pub resources: AppBehaviorResourceCeiling,
    pub reviewed_request_digest: AppDigest,
}

/// Closed app-visible terminal outcomes. This is deliberately not the
/// transport-event enum: app ingress is derived only from admitted canonical
/// runtime facts and exposes this stable projection vocabulary.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppEventTerminalOutcomeV1 {
    Succeeded,
    Failed,
    /// Reserved until a post-persistence canonical cancellation producer is
    /// available. Current V1 manifest/grant validation refuses this value.
    Cancelled,
}

/// Closed V1 subscription grammar. Same-installation scope is part of the
/// variant contract, not a package-selectable predicate, so a declaration
/// cannot acquire ambient workspace event-read authority.
#[derive(
    Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppEventSubscriptionV1 {
    InstallationExecutionTerminal {
        outcomes: Vec<AppEventTerminalOutcomeV1>,
    },
}

/// Exact requested or owner-narrowed authority for one event-driven behavior.
/// The source/filter and host projection are immutable review material; an
/// owner may only slow dispatch or lower numeric resource ceilings.
#[derive(
    Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(deny_unknown_fields)]
pub struct AppEventBehaviorGrant {
    pub event_behavior_id: AppName,
    pub purpose: String,
    pub action: AppName,
    pub subscription: AppEventSubscriptionV1,
    pub subscription_digest: AppDigest,
    pub projection_schema_digest: AppDigest,
    pub operations: Vec<AppName>,
    /// Digest of the ordered operation recipe. See
    /// [`AppBehaviorGrant::steps_digest`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema_digest: Option<AppDigest>,
    pub min_interval_seconds: u64,
    pub resources: AppBehaviorResourceCeiling,
    pub reviewed_request_digest: AppDigest,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppNotificationKindV1 {
    Briefing,
    Escalation,
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum AppNotificationSeverityV1 {
    Info,
    Warning,
    /// Reserved vocabulary. The current durable notification owner persists
    /// only info/warning, so V1 declaration and grant validation refuse it.
    Critical,
}

impl AppNotificationSeverityV1 {
    fn rank(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Critical => 2,
        }
    }
}

/// Workflow-local one-way owner-notification authority. Correlation,
/// idempotency, request type and installation scope remain host-sealed runtime
/// material and therefore cannot be substituted through this grant.
#[derive(
    Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
#[serde(deny_unknown_fields)]
pub struct AppNotificationGrant {
    pub workflow_id: AppName,
    pub port_id: AppName,
    pub purpose: String,
    pub kind: AppNotificationKindV1,
    pub severity_ceiling: AppNotificationSeverityV1,
    pub max_notifications_per_period: u32,
    pub period_seconds: u64,
    pub max_pending: u16,
    pub ttl_seconds: u64,
    pub reviewed_request_digest: AppDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppNetworkPolicy {
    Denied,
    ApprovedDestinations { destinations: Vec<AppReference> },
}

/// Integer ceilings avoid floating-point accounting drift. Monetary values are
/// represented as micro-USD until the canonical resource authority settles
/// exact provider observations in Phase 4B.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceCeiling {
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub max_cost_microusd: u64,
    pub max_paid_tool_invocations: u64,
    pub max_active_seconds: u64,
    pub max_lifetime_seconds: u64,
    pub max_browser_network_actions: u64,
    pub max_concurrent_foreground_runs: u16,
    pub max_concurrent_background_runs: u16,
    pub max_records: u64,
    pub max_payload_bytes: u64,
    pub max_attachment_bytes: u64,
    pub max_monthly_tokens: u64,
    pub max_monthly_cost_microusd: u64,
}

/// One owner-granted scripted custom-surface entry point (plan 1.6),
/// frozen exactly as it was narrowed at approval: the route the surface
/// mounts at, the `surfaces/` HTML document the frame loads, and that
/// document's verified content digest from the immutable reviewed bundle.
/// The digest is the runtime fence — the scripted host admits a plan only
/// when the live entry document still hashes to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppGrantedCustomSurfaceEntryPoint {
    pub route: String,
    pub document: String,
    pub document_digest: AppDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppGrantRevision {
    pub installation_id: AppInstallationId,
    pub revision: AppRevision,
    pub package_revision_ref: AppReference,
    pub requested_tools: Vec<AppReference>,
    pub granted_tools: Vec<AppReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_agents: Vec<AppReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_agents: Vec<AppReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_personalities: Vec<AppReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_personalities: Vec<AppReference>,
    /// Exact package-lock requests shown to the owner. Each entry grants its
    /// complete request and exists independently of the selected subset below.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_interactive_capabilities: Vec<AppReviewedInteractiveCapabilityGrant>,
    /// Explicit owner-selected subset/narrowing. Empty means no interactive
    /// physical authority, never implicit all-actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_interactive_capabilities: Vec<AppReviewedInteractiveCapabilityGrant>,
    /// Owner-granted scripted custom-surface entry points (plan 1.6),
    /// persisted exactly as narrowed at approval. Empty or absent grants
    /// NO surfaces — the grant is never implicit-all — and the scripted
    /// host compiles a plan only for an entry whose live document still
    /// matches the granted digest. `default` + `skip_serializing_if` keep
    /// pre-1.6 persisted grant revisions decoding unchanged and their
    /// canonical bytes identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_custom_surface_entry_points: Vec<AppGrantedCustomSurfaceEntryPoint>,
    /// Exact reviewed behavior requests and their owner-selected subset.
    /// Empty/default keeps every pre-`app_behaviors_v1` durable record and
    /// authority digest byte-compatible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_behavior_grants: Vec<AppBehaviorGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_behavior_grants: Vec<AppBehaviorGrant>,
    /// Exact event-behavior and notification requests plus explicit
    /// owner-selected subsets. Empty is deny-all. Default omission preserves
    /// pre-feature durable records and canonical authority identities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_event_behavior_grants: Vec<AppEventBehaviorGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_event_behavior_grants: Vec<AppEventBehaviorGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_notification_grants: Vec<AppNotificationGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted_notification_grants: Vec<AppNotificationGrant>,
    /// The manifest's `app_memory_read_v1` request and the owner's reviewed
    /// grant (split interactive / background). `None` on both means the app
    /// reads no owner memory; absence is skipped so pre-feature records and
    /// authority digests stay byte-identical. Later owner edits live in the
    /// registry's memory-grant head row, not here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_memory_read: Option<super::memory_access::AppMemoryReadRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_memory_read: Option<super::memory_access::AppMemoryReadGrant>,
    /// `app_secret_use_v1`: the secrets the app's locked tools asked to use
    /// and the (tool, secret) pairs the owner ticked. `None` on both means no
    /// tool uses a secret; absence is skipped so older records and authority
    /// digests stay byte-identical. An empty grant is explicit refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_secret_uses: Option<Vec<super::secret_access::AppSecretUseRequest>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_secret_uses: Option<Vec<super::secret_access::AppSecretUseGrant>>,
    /// `app_in_place_skill_v1`: the owner's explicit "any public host" grant.
    /// An in-place tool (or one that declares hosts) may then reach any
    /// public HTTPS host through its call's broker, and its ticked keys may
    /// travel to any of them. Off by default and only set by an explicit
    /// owner choice at approval; `false` is skipped so older records and
    /// authority digests stay byte-identical.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub granted_any_public_host: bool,
    pub requested_context_reads: Vec<AppReference>,
    pub granted_context_reads: Vec<AppReference>,
    pub requested_personal_agent_data_access: Vec<AppEntityProjectionGrant>,
    pub granted_personal_agent_data_access: Vec<AppEntityProjectionGrant>,
    pub requested_data_handling_policy: AppDataHandlingPolicy,
    pub granted_data_handling_policy: AppDataHandlingPolicy,
    pub granted_data_handling_policy_digest: AppDigest,
    pub requested_background_execution: AppBackgroundExecution,
    pub granted_background_execution: AppBackgroundExecution,
    pub requested_network_policy: AppNetworkPolicy,
    pub granted_network_policy: AppNetworkPolicy,
    pub requested_resource_ceiling: AppResourceCeiling,
    pub granted_resource_ceiling: AppResourceCeiling,
    pub approved_by: AppReference,
    pub approved_at: DateTime<Utc>,
    pub authority_digest: AppDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppSchemaCompatibility {
    Initial,
    Compatible,
    MigrationRequired,
    Incompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppSchemaRevision {
    pub installation_id: AppInstallationId,
    pub revision: AppRevision,
    pub package_revision_ref: AppReference,
    pub canonical_entity_schema: Value,
    pub canonical_data_handling_policy: AppDataHandlingPolicy,
    pub compiled_validation_schema: Value,
    pub compiled_index_plan: Value,
    pub compatibility_with_previous: AppSchemaCompatibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_plan_ref: Option<AppReference>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppRecordActorKind {
    User,
    Agent,
    Workflow,
    Surface,
    Migration,
    Import,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecordProvenance {
    pub actor_kind: AppRecordActorKind,
    pub actor_id: AppReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_revision: Option<AppRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_receipt_id: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_artifact_refs: Vec<AppReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citation_refs: Vec<AppReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppRecordRevision {
    pub installation_id: AppInstallationId,
    pub entity_name: AppName,
    pub record_id: AppRecordId,
    pub record_revision: AppRevision,
    pub dataset_generation: u64,
    pub schema_revision: AppRevision,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handling_override: Option<AppDataHandlingPolicy>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
    pub provenance: AppRecordProvenance,
}

/// Content-free resource-ledger dimension for one scheduler-minted behavior.
///
/// Fields are private so callers cannot assemble a partial identity. The
/// workflow owner seals this only from an exact, currently granted behavior;
/// the resource authority later revalidates the dimension digest before using
/// it to select behavior-local period totals. The two monthly ceilings remain
/// explicit because the app-wide ceiling is evaluated independently.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppResourceBehaviorIdentity {
    behavior_id: AppName,
    reviewed_request_digest: AppDigest,
    granted_behavior_digest: AppDigest,
    max_monthly_tokens: u64,
    max_monthly_cost_microusd: u64,
    authority_binding_digest: AppDigest,
    ledger_dimension_digest: AppDigest,
}

impl AppResourceBehaviorIdentity {
    pub(crate) fn seal_scheduler_behavior(
        grant: &AppBehaviorGrant,
    ) -> Result<Self, AppContractError> {
        let granted_behavior_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(grant).map_err(|error| {
                AppContractError::invalid("behavior_resource_identity", error.to_string())
            })?)
            .map_err(|error| {
                AppContractError::invalid("behavior_resource_identity", error.to_string())
            })?;
        let authority_binding_digest = app_resource_behavior_authority_digest(
            &grant.behavior_id,
            &grant.reviewed_request_digest,
            &granted_behavior_digest,
            grant.resources.max_tokens_per_month,
            grant.resources.max_cost_microusd_per_month,
        )?;
        // Owner narrowing must not reset already consumed monthly spend. The
        // durable grouping key therefore binds the stable requested behavior
        // identity, while `authority_binding_digest` independently binds the
        // exact granted limits enforced for this root.
        let ledger_dimension_digest = app_resource_behavior_dimension_digest(
            &grant.behavior_id,
            &grant.reviewed_request_digest,
        )?;
        Ok(Self {
            behavior_id: grant.behavior_id.clone(),
            reviewed_request_digest: grant.reviewed_request_digest.clone(),
            granted_behavior_digest,
            max_monthly_tokens: grant.resources.max_tokens_per_month,
            max_monthly_cost_microusd: grant.resources.max_cost_microusd_per_month,
            authority_binding_digest,
            ledger_dimension_digest,
        })
    }

    /// Seal the same behavior-local resource dimension for an event-owned
    /// launch. Event request digests are domain-separated from scheduled
    /// request digests, so the stable ledger dimension cannot collide even
    /// when a package reuses the same local identifier on both axes.
    pub(crate) fn seal_event_behavior(
        grant: &AppEventBehaviorGrant,
    ) -> Result<Self, AppContractError> {
        let granted_behavior_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(grant).map_err(|error| {
                AppContractError::invalid("behavior_resource_identity", error.to_string())
            })?)
            .map_err(|error| {
                AppContractError::invalid("behavior_resource_identity", error.to_string())
            })?;
        let authority_binding_digest = app_resource_behavior_authority_digest(
            &grant.event_behavior_id,
            &grant.reviewed_request_digest,
            &granted_behavior_digest,
            grant.resources.max_tokens_per_month,
            grant.resources.max_cost_microusd_per_month,
        )?;
        let ledger_dimension_digest = app_resource_behavior_dimension_digest(
            &grant.event_behavior_id,
            &grant.reviewed_request_digest,
        )?;
        Ok(Self {
            behavior_id: grant.event_behavior_id.clone(),
            reviewed_request_digest: grant.reviewed_request_digest.clone(),
            granted_behavior_digest,
            max_monthly_tokens: grant.resources.max_tokens_per_month,
            max_monthly_cost_microusd: grant.resources.max_cost_microusd_per_month,
            authority_binding_digest,
            ledger_dimension_digest,
        })
    }

    pub(crate) fn ledger_dimension_digest(&self) -> &AppDigest {
        &self.ledger_dimension_digest
    }

    pub(crate) fn max_monthly_tokens(&self) -> u64 {
        self.max_monthly_tokens
    }

    pub(crate) fn max_monthly_cost_microusd(&self) -> u64 {
        self.max_monthly_cost_microusd
    }
}

fn app_resource_behavior_dimension_digest(
    behavior_id: &AppName,
    reviewed_request_digest: &AppDigest,
) -> Result<AppDigest, AppContractError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-resource-behavior-ledger.v1",
        "behavior_id": behavior_id,
        "reviewed_request_digest": reviewed_request_digest,
    }))
    .map_err(|error| AppContractError::invalid("behavior_resource_identity", error.to_string()))
}

fn app_resource_behavior_authority_digest(
    behavior_id: &AppName,
    reviewed_request_digest: &AppDigest,
    granted_behavior_digest: &AppDigest,
    max_monthly_tokens: u64,
    max_monthly_cost_microusd: u64,
) -> Result<AppDigest, AppContractError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-resource-behavior-authority.v1",
        "behavior_id": behavior_id,
        "reviewed_request_digest": reviewed_request_digest,
        "granted_behavior_digest": granted_behavior_digest,
        "max_monthly_tokens": max_monthly_tokens,
        "max_monthly_cost_microusd": max_monthly_cost_microusd,
    }))
    .map_err(|error| AppContractError::invalid("behavior_resource_identity", error.to_string()))
}

impl ValidateAppContract for AppResourceBehaviorIdentity {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.max_monthly_tokens == 0 || self.max_monthly_cost_microusd == 0 {
            return Err(AppContractError::invalid(
                "behavior_resource_identity",
                "monthly ceilings must be greater than zero",
            ));
        }
        let expected_authority = app_resource_behavior_authority_digest(
            &self.behavior_id,
            &self.reviewed_request_digest,
            &self.granted_behavior_digest,
            self.max_monthly_tokens,
            self.max_monthly_cost_microusd,
        )?;
        let expected_dimension = app_resource_behavior_dimension_digest(
            &self.behavior_id,
            &self.reviewed_request_digest,
        )?;
        if expected_authority != self.authority_binding_digest
            || expected_dimension != self.ledger_dimension_digest
        {
            return Err(AppContractError::invalid(
                "behavior_resource_identity",
                "ledger or authority digest does not match the sealed behavior grant",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRunBinding {
    pub scope: AppScope,
    pub installation_id: AppInstallationId,
    pub package_revision_ref: AppReference,
    pub workflow_id: AppName,
    /// Present only on scheduler-minted `app_behaviors_v1` roots. Omission is
    /// the legacy/ordinary app-task identity and retains installation-wide
    /// period accounting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_resource_identity: Option<AppResourceBehaviorIdentity>,
    pub execution_id: AppReference,
    pub resolved_agent_id: AppReference,
    pub authority_digest: AppDigest,
    pub budget_ledger_ref: AppReference,
    pub schema_revision: AppRevision,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppMutationOrigin {
    OwnerApi {
        session_ref: AppReference,
        request_ref: AppReference,
    },
    Workflow {
        execution_id: AppReference,
        output_revision: AppRevision,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        source_artifact_refs: Vec<AppReference>,
    },
    Surface {
        surface_session_id: AppReference,
        client_mutation_id: AppReference,
    },
    Migration {
        migration_run_id: AppReference,
        migration_batch: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct AppCommittedRecordRevision {
    pub entity: AppName,
    pub record_id: AppRecordId,
    pub revision: AppRevision,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppChangeSequenceRange {
    pub first: u64,
    pub last: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMutationReceipt {
    pub receipt_id: AppReference,
    pub installation_id: AppInstallationId,
    pub origin: AppMutationOrigin,
    pub mutation_key: AppDigest,
    pub batch_digest: AppDigest,
    pub committed_record_revisions: Vec<AppCommittedRecordRevision>,
    pub change_seq_range: AppChangeSequenceRange,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppSurfaceStatus {
    Active,
    Disabled,
    Quarantined,
    Retired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppSurfaceBinding {
    pub installation_id: AppInstallationId,
    pub surface_revision: AppRevision,
    pub package_revision_ref: AppReference,
    pub app_local_route: String,
    pub canonical_host_route: String,
    pub view_id: AppName,
    pub compiled_view_digest: AppDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_surface_ref: Option<AppReference>,
    pub status: AppSurfaceStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppDisclosureProviderClass {
    Deterministic,
    LocalModel,
    RemoteModel,
    ExternalTool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppApprovedRecordProjection {
    pub entity: AppName,
    pub fields: Vec<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub record_revisions: Vec<AppReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDisclosureEnvelope {
    pub disclosure_id: AppReference,
    pub scope: AppScope,
    pub installation_id: AppInstallationId,
    pub execution_id: AppReference,
    pub purpose: AppName,
    pub provider_class: AppDisclosureProviderClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_endpoint_ref: Option<AppReference>,
    pub package_revision_ref: AppReference,
    pub grant_revision: AppRevision,
    pub schema_revision: AppRevision,
    pub approved_projections: Vec<AppApprovedRecordProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<AppReference>,
    pub max_rows: u32,
    pub max_bytes: u64,
    pub max_tokens: u64,
    pub max_nodes: u32,
    pub max_relation_depth: u16,
    pub redaction_policy_digest: AppDigest,
    pub content_projection_digest: AppDigest,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReviewedWorkflowMaterialBinding {
    pub schema: String,
    pub workflow_id: AppName,
    pub agent_ref: AppReference,
    pub agent_definition_revision: AppRevision,
    pub agent_definition_digest: AppDigest,
    pub agent_descriptor_digest: AppDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality_content_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality_descriptor_digest: Option<AppDigest>,
    pub binding_digest: AppDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInstallationApproval {
    pub approval_id: AppReference,
    pub revision: AppRevision,
    pub authenticated_scope_ref: AppScopeBindingRef,
    pub actor_ref: AppReference,
    pub session_ref: AppReference,
    pub authentication: AppApprovalAuthentication,
    pub authentication_revision: AppRevision,
    pub install_or_update_attempt_id: AppReference,
    pub package_content_digest: AppDigest,
    pub requested_authority_digest: AppDigest,
    pub granted_authority_digest: AppDigest,
    pub data_policy_diff_digest: AppDigest,
    pub resource_diff_digest: AppDigest,
    pub schema_diff_digest: AppDigest,
    pub migration_diff_digest: AppDigest,
    pub global_policy_revision: AppRevision,
    /// Exact mutable scope-owned instruction identities shown to the owner at
    /// review. New launches must match one workflow binding before task-local
    /// material can be sealed. Empty is legacy-readable but not launchable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflow_material_bindings: Vec<AppReviewedWorkflowMaterialBinding>,
    /// Closed, owner-approved contribution ceilings. These remain inert until
    /// rebound to an exact task through the package lock and current grant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contribution_grants: Vec<AppReviewedContributionPortGrant>,
    /// Exact owner-reviewed interactive grants consumed by this approval.
    /// Each entry retains both the package request and the granted narrowing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interactive_capability_grants: Vec<AppReviewedInteractiveCapabilityGrant>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_installation_revision: Option<AppRevision>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppApprovalAuthentication {
    AuthenticatedSession,
    TrustedLoopbackSingleUser,
    /// Server-owned review of one exact digest-pinned system installation.
    /// This value can be deserialized from durable audit records, but the live
    /// authentication scope that creates it is not transport-constructible.
    TrustedSystemPackageHost,
}

impl ValidateAppContract for AppPackageRevision {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        semver::Version::parse(&self.semantic_version)
            .map_err(|error| AppContractError::invalid("semantic_version", error.to_string()))?;
        parse_exact_version_label("manifest_schema_version", &self.manifest_schema_version)?;
        parse_exact_version_label("authoring_sdk_version", &self.authoring_sdk_version)?;
        validate_nonempty_bounded(
            "compatibility",
            self.compatibility.len(),
            limits.max_collection_items(),
        )?;
        let mut contracts = HashSet::with_capacity(self.compatibility.len());
        for compatibility in &self.compatibility {
            if !contracts.insert(&compatibility.contract) {
                return Err(AppContractError::invalid(
                    "compatibility",
                    "contains a duplicate contract",
                ));
            }
            parse_version_requirement("compatibility.requirement", &compatibility.requirement)?;
        }
        Ok(())
    }
}

impl ValidateAppContract for AppPackageDirectoryMetadata {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.description.len() > 2_048 {
            return Err(AppContractError::invalid(
                "description",
                "must not exceed 2048 bytes",
            ));
        }
        if self.actions.len() > limits.max_collection_items() {
            return Err(AppContractError::invalid(
                "actions",
                "contains too many declared actions",
            ));
        }
        if self.custom_surface_entry_count > APP_CUSTOM_SURFACE_MAX_ENTRY_POINTS {
            return Err(AppContractError::invalid(
                "custom_surface_entry_count",
                "exceeds the declared custom-surface entry-point bound",
            ));
        }
        if self.navigation.len() > APP_NAVIGATION_MAX_ENTRIES {
            return Err(AppContractError::invalid(
                "navigation",
                "exceeds the first-party navigation declaration bound",
            ));
        }
        // Clients refuse a whole directory entry whose navigation is off shape
        // rather than mounting half a declared list, so a durable record that
        // could not survive that parse would make the app vanish from the
        // directory altogether. Re-check the manifest's own rules on the way in
        // and out of the store instead of trusting the admitting writer.
        let mut navigation_ids = HashSet::with_capacity(self.navigation.len());
        let mut navigation_routes = HashSet::with_capacity(self.navigation.len());
        for entry in &self.navigation {
            if entry.title.trim().is_empty()
                || entry.title.len() > APP_SURFACING_MAX_TITLE_BYTES
                || entry.title.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(AppContractError::invalid(
                    "navigation.title",
                    "must be bounded non-control text",
                ));
            }
            if entry.route.parameter_names().next().is_some() {
                return Err(AppContractError::invalid(
                    "navigation.route",
                    "must not be a dynamic first-party route",
                ));
            }
            if !navigation_ids.insert(&entry.id) || !navigation_routes.insert(&entry.route) {
                return Err(AppContractError::invalid(
                    "navigation",
                    "contains a duplicate navigation id or first-party route",
                ));
            }
        }
        let mut unique = HashSet::with_capacity(self.actions.len());
        if self.actions.iter().any(|action| !unique.insert(action)) {
            return Err(AppContractError::invalid(
                "actions",
                "contains a duplicate declared action",
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppInstallation {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        self.lifecycle
            .validate()
            .map_err(|error| AppContractError::invalid("lifecycle", error.to_string()))?;
        ensure_time_order("updated_at", &self.created_at, &self.updated_at)?;
        let any_active_revision = self.grant_revision.is_some()
            || self.active_schema_revision.is_some()
            || self.active_surface_revision.is_some();
        let all_active_revisions = self.grant_revision.is_some()
            && self.active_schema_revision.is_some()
            && self.active_surface_revision.is_some();
        match self.lifecycle.status {
            AppInstallationStatus::ReadyForReview if any_active_revision => {
                return Err(AppContractError::invalid(
                    "lifecycle",
                    "ready_for_review cannot expose active grant/schema/surface revisions",
                ));
            },
            AppInstallationStatus::Enabled
            | AppInstallationStatus::Disabled
            | AppInstallationStatus::UpdatePending
            | AppInstallationStatus::Quarantined
            | AppInstallationStatus::UninstalledRetained
                if !all_active_revisions =>
            {
                return Err(AppContractError::invalid(
                    "lifecycle",
                    "post-review states require active grant, schema and surface revisions",
                ));
            },
            AppInstallationStatus::Disabled if self.disabled_at.is_none() => {
                return Err(AppContractError::invalid(
                    "disabled_at",
                    "is required when status is disabled",
                ));
            },
            AppInstallationStatus::Quarantined if self.quarantined_at.is_none() => {
                return Err(AppContractError::invalid(
                    "quarantined_at",
                    "is required when status is quarantined",
                ));
            },
            AppInstallationStatus::UninstalledRetained if self.uninstalled_at.is_none() => {
                return Err(AppContractError::invalid(
                    "uninstalled_at",
                    "is required when status is uninstalled_retained",
                ));
            },
            AppInstallationStatus::Purged if self.purged_at.is_none() => {
                return Err(AppContractError::invalid(
                    "purged_at",
                    "is required when status is purged",
                ));
            },
            _ => {},
        }
        for (field, timestamp) in [
            ("disabled_at", self.disabled_at.as_ref()),
            ("quarantined_at", self.quarantined_at.as_ref()),
            ("uninstalled_at", self.uninstalled_at.as_ref()),
            ("purged_at", self.purged_at.as_ref()),
        ] {
            if let Some(timestamp) = timestamp {
                ensure_time_order(field, &self.created_at, timestamp)?;
            }
        }
        Ok(())
    }
}

impl ValidateAppContract for AppLifecycleAttempt {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        ensure_time_order("updated_at", &self.created_at, &self.updated_at)?;
        match self.kind {
            AppLifecycleAttemptKind::InitialInstall => {
                if self.installation_id.is_some() || self.source_installation_generation.is_some() {
                    return Err(AppContractError::invalid(
                        "kind",
                        "initial_install cannot bind an existing installation generation",
                    ));
                }
            },
            AppLifecycleAttemptKind::Reinstall | AppLifecycleAttemptKind::Update => {
                if self.installation_id.is_none()
                    || self
                        .source_installation_generation
                        .is_none_or(|generation| generation == 0)
                {
                    return Err(AppContractError::invalid(
                        "kind",
                        "reinstall/update requires an installation and positive source generation",
                    ));
                }
            },
        }
        match self.state {
            AppLifecycleAttemptState::ReadyForReview | AppLifecycleAttemptState::Committed
                if self.conformance_attestation_ref.is_none() =>
            {
                return Err(AppContractError::invalid(
                    "conformance_attestation_ref",
                    "is required after conformance",
                ));
            },
            AppLifecycleAttemptState::Committed if self.approval_ref.is_none() => {
                return Err(AppContractError::invalid(
                    "approval_ref",
                    "is required for a committed attempt",
                ));
            },
            AppLifecycleAttemptState::Failed if self.failure_code.is_none() => {
                return Err(AppContractError::invalid(
                    "failure_code",
                    "is required for a failed attempt",
                ));
            },
            _ => {},
        }
        if self.state != AppLifecycleAttemptState::Failed && self.failure_code.is_some() {
            return Err(AppContractError::invalid(
                "failure_code",
                "is valid only for a failed attempt",
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppGrantRevision {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        // A stored memory grant must narrow its stored request (defence in
        // depth behind the review path, so a tampered row cannot widen it).
        match (&self.requested_memory_read, &self.granted_memory_read) {
            (_, None) => {},
            (None, Some(_)) => {
                return Err(AppContractError::invalid(
                    "granted_memory_read",
                    "a memory grant requires a memory request",
                ))
            },
            (Some(request), Some(grant)) => {
                super::memory_access::validate_memory_read_grant(grant, request)
                    .map_err(|reason| AppContractError::invalid("granted_memory_read", reason))?;
            },
        }
        // The same narrowing rule for secret use: a stored grant must be the
        // canonical (sorted, unique) subset of its stored request.
        match (&self.requested_secret_uses, &self.granted_secret_uses) {
            (_, None) => {},
            (None, Some(_)) => {
                return Err(AppContractError::invalid(
                    "granted_secret_uses",
                    "a secret-use grant requires a secret-use request",
                ))
            },
            (Some(requests), Some(grants)) => {
                // Tolerant of an unscoped key stored before per-key scopes:
                // it grants nothing until re-reviewed.
                let canonical = super::secret_access::validate_stored_secret_use_grant(
                    requests,
                    grants,
                    &super::secret_access::network_policy_hosts(&self.granted_network_policy),
                    self.granted_data_handling_policy.external_egress
                        == AppExternalEgress::AnyPublicHost,
                )
                .map_err(|error| {
                    AppContractError::invalid("granted_secret_uses", error.to_string())
                })?;
                if &canonical != grants {
                    return Err(AppContractError::invalid(
                        "granted_secret_uses",
                        "a secret-use grant must be sorted and unique",
                    ));
                }
            },
        }
        validate_unique_refs("requested_tools", &self.requested_tools, limits)?;
        validate_unique_refs("granted_tools", &self.granted_tools, limits)?;
        validate_unique_refs(
            "requested_context_reads",
            &self.requested_context_reads,
            limits,
        )?;
        validate_unique_refs("granted_context_reads", &self.granted_context_reads, limits)?;
        ensure_ref_subset("granted_tools", &self.granted_tools, &self.requested_tools)?;
        validate_unique_refs("requested_agents", &self.requested_agents, limits)?;
        validate_unique_refs("granted_agents", &self.granted_agents, limits)?;
        ensure_ref_subset(
            "granted_agents",
            &self.granted_agents,
            &self.requested_agents,
        )?;
        validate_unique_refs(
            "requested_personalities",
            &self.requested_personalities,
            limits,
        )?;
        validate_unique_refs("granted_personalities", &self.granted_personalities, limits)?;
        ensure_ref_subset(
            "granted_personalities",
            &self.granted_personalities,
            &self.requested_personalities,
        )?;
        validate_interactive_capability_grants(
            "requested_interactive_capabilities",
            &self.requested_interactive_capabilities,
            limits,
        )?;
        validate_interactive_capability_grants(
            "granted_interactive_capabilities",
            &self.granted_interactive_capabilities,
            limits,
        )?;
        validate_granted_custom_surface_entry_points(
            &self.granted_custom_surface_entry_points,
            limits,
        )?;
        validate_behavior_grants(
            &self.requested_behavior_grants,
            &self.granted_behavior_grants,
            &self.requested_background_execution,
            &self.granted_background_execution,
            &self.requested_resource_ceiling,
            &self.granted_resource_ceiling,
            limits,
        )?;
        validate_event_behavior_grants(
            &self.requested_event_behavior_grants,
            &self.granted_event_behavior_grants,
            &self.requested_background_execution,
            &self.granted_background_execution,
            &self.requested_resource_ceiling,
            &self.granted_resource_ceiling,
            limits,
        )?;
        validate_notification_grants(
            &self.requested_notification_grants,
            &self.granted_notification_grants,
            limits,
        )?;
        let requested_interactive = self
            .requested_interactive_capabilities
            .iter()
            .map(|grant| (grant.dependency_ref(), grant))
            .collect::<HashMap<_, _>>();
        for granted in &self.granted_interactive_capabilities {
            let requested = requested_interactive
                .get(granted.dependency_ref())
                .ok_or_else(|| {
                    AppContractError::invalid(
                        "granted_interactive_capabilities",
                        "contains a dependency that was not requested",
                    )
                })?;
            if granted.locked_binding_digest() != requested.locked_binding_digest()
                || granted.primitive_binding_digest() != requested.primitive_binding_digest()
                || granted.requested_request_digest() != requested.requested_request_digest()
                || !granted.granted().narrows(requested.requested())
            {
                return Err(AppContractError::invalid(
                    "granted_interactive_capabilities",
                    "widens or substitutes the exact reviewed request/lock binding",
                ));
            }
        }
        ensure_ref_subset(
            "granted_context_reads",
            &self.granted_context_reads,
            &self.requested_context_reads,
        )?;
        validate_projection_grants(
            "requested_personal_agent_data_access",
            &self.requested_personal_agent_data_access,
            limits,
        )?;
        validate_projection_grants(
            "granted_personal_agent_data_access",
            &self.granted_personal_agent_data_access,
            limits,
        )?;
        ensure_projection_subset(
            &self.granted_personal_agent_data_access,
            &self.requested_personal_agent_data_access,
        )?;
        validate_policy(&self.requested_data_handling_policy, limits)?;
        validate_policy(&self.granted_data_handling_policy, limits)?;
        ensure_policy_narrows(
            &self.granted_data_handling_policy,
            &self.requested_data_handling_policy,
        )?;
        validate_background_execution(
            "requested_background_execution",
            &self.requested_background_execution,
        )?;
        validate_background_execution(
            "granted_background_execution",
            &self.granted_background_execution,
        )?;
        ensure_background_narrows(
            &self.granted_background_execution,
            &self.requested_background_execution,
        )?;
        validate_network_policy(&self.requested_network_policy, limits)?;
        validate_network_policy(&self.granted_network_policy, limits)?;
        ensure_network_narrows(&self.granted_network_policy, &self.requested_network_policy)?;
        validate_resource_ceiling(
            "requested_resource_ceiling",
            &self.requested_resource_ceiling,
        )?;
        validate_resource_ceiling("granted_resource_ceiling", &self.granted_resource_ceiling)?;
        ensure_resource_narrows(
            &self.granted_resource_ceiling,
            &self.requested_resource_ceiling,
        )?;
        ensure_background_within_resource_ceiling(
            "requested_background_execution",
            &self.requested_background_execution,
            &self.requested_resource_ceiling,
        )?;
        ensure_background_within_resource_ceiling(
            "granted_background_execution",
            &self.granted_background_execution,
            &self.granted_resource_ceiling,
        )?;
        if self
            .revoked_at
            .as_ref()
            .is_some_and(|revoked_at| revoked_at < &self.approved_at)
        {
            return Err(AppContractError::invalid(
                "revoked_at",
                "cannot precede approved_at",
            ));
        }
        Ok(())
    }
}

fn validate_interactive_capability_grants(
    field: &'static str,
    grants: &[AppReviewedInteractiveCapabilityGrant],
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    if grants.len() > limits.max_collection_items() {
        return Err(AppContractError::invalid(
            field,
            "exceeds the collection ceiling",
        ));
    }
    let mut previous: Option<&AppReference> = None;
    for grant in grants {
        grant
            .validate()
            .map_err(|error| AppContractError::invalid(field, error.to_string()))?;
        if previous.is_some_and(|prior| prior >= grant.dependency_ref()) {
            return Err(AppContractError::invalid(
                field,
                "is duplicated or not in canonical dependency order",
            ));
        }
        previous = Some(grant.dependency_ref());
    }
    Ok(())
}

pub fn app_behavior_request_digest(
    behavior_id: &AppName,
    purpose: &str,
    action: &AppName,
    input_selector_digest: &AppDigest,
    operations: &[AppName],
    steps_digest: Option<&AppDigest>,
    output_schema_digest: Option<&AppDigest>,
    min_interval_seconds: u64,
    resources: &AppBehaviorResourceCeiling,
) -> Result<AppDigest, AppContractError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-behavior-review.v1",
        "behavior_id": behavior_id,
        "purpose": purpose,
        "action": action,
        "input_selector_digest": input_selector_digest,
        "operations": operations,
        "steps_digest": steps_digest,
        "output_schema_digest": output_schema_digest,
        "min_interval_seconds": min_interval_seconds,
        "resources": resources,
    }))
    .map_err(|error| AppContractError::invalid("behavior_request_digest", error.to_string()))
}

pub fn app_event_subscription_digest(
    subscription: &AppEventSubscriptionV1,
) -> Result<AppDigest, AppContractError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-event-subscription.v1",
        "subscription": subscription,
    }))
    .map_err(|error| AppContractError::invalid("event_subscription_digest", error.to_string()))
}

#[allow(clippy::too_many_arguments)]
pub fn app_event_behavior_request_digest(
    event_behavior_id: &AppName,
    purpose: &str,
    action: &AppName,
    subscription_digest: &AppDigest,
    projection_schema_digest: &AppDigest,
    operations: &[AppName],
    steps_digest: Option<&AppDigest>,
    output_schema_digest: Option<&AppDigest>,
    min_interval_seconds: u64,
    resources: &AppBehaviorResourceCeiling,
) -> Result<AppDigest, AppContractError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-event-behavior-review.v1",
        "event_behavior_id": event_behavior_id,
        "purpose": purpose,
        "action": action,
        "subscription_digest": subscription_digest,
        "projection_schema_digest": projection_schema_digest,
        "operations": operations,
        "steps_digest": steps_digest,
        "output_schema_digest": output_schema_digest,
        "min_interval_seconds": min_interval_seconds,
        "resources": resources,
    }))
    .map_err(|error| AppContractError::invalid("event_behavior_request_digest", error.to_string()))
}

#[allow(clippy::too_many_arguments)]
pub fn app_notification_request_digest(
    workflow_id: &AppName,
    port_id: &AppName,
    purpose: &str,
    kind: AppNotificationKindV1,
    severity_ceiling: AppNotificationSeverityV1,
    max_notifications_per_period: u32,
    period_seconds: u64,
    max_pending: u16,
    ttl_seconds: u64,
) -> Result<AppDigest, AppContractError> {
    AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-owner-notification-review.v1",
        "workflow_id": workflow_id,
        "port_id": port_id,
        "purpose": purpose,
        "kind": kind,
        "severity_ceiling": severity_ceiling,
        "max_notifications_per_period": max_notifications_per_period,
        "period_seconds": period_seconds,
        "max_pending": max_pending,
        "ttl_seconds": ttl_seconds,
    }))
    .map_err(|error| AppContractError::invalid("notification_request_digest", error.to_string()))
}

fn validate_behavior_grants(
    requested: &[AppBehaviorGrant],
    granted: &[AppBehaviorGrant],
    requested_background: &AppBackgroundExecution,
    granted_background: &AppBackgroundExecution,
    requested_app_resources: &AppResourceCeiling,
    granted_app_resources: &AppResourceCeiling,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    if requested.len() > limits.max_collection_items()
        || granted.len() > limits.max_collection_items()
    {
        return Err(AppContractError::invalid(
            "behavior_grants",
            "exceeds the collection ceiling",
        ));
    }
    let mut requested_by_id = HashMap::new();
    let mut previous_requested_id: Option<&AppName> = None;
    for request in requested {
        validate_behavior_grant_shape("requested_behavior_grants", request)?;
        if request.resources.period_seconds < request.min_interval_seconds
            || u64::from(request.resources.max_starts_per_period)
                > request
                    .resources
                    .period_seconds
                    .div_ceil(request.min_interval_seconds)
        {
            return Err(AppContractError::invalid(
                "requested_behavior_grants",
                "requested frequency period or cap is inconsistent with the manifest cadence",
            ));
        }
        if previous_requested_id.is_some_and(|previous| previous >= &request.behavior_id) {
            return Err(AppContractError::invalid(
                "requested_behavior_grants",
                "is duplicated or not in canonical behavior-id order",
            ));
        }
        previous_requested_id = Some(&request.behavior_id);
        if requested_by_id
            .insert(request.behavior_id.clone(), request)
            .is_some()
        {
            return Err(AppContractError::invalid(
                "requested_behavior_grants",
                "contains duplicate behavior ids",
            ));
        }
        let expected_digest = app_behavior_request_digest(
            &request.behavior_id,
            &request.purpose,
            &request.action,
            &request.input_selector_digest,
            &request.operations,
            request.steps_digest.as_ref(),
            request.output_schema_digest.as_ref(),
            request.min_interval_seconds,
            &request.resources,
        )?;
        if expected_digest != request.reviewed_request_digest {
            return Err(AppContractError::invalid(
                "requested_behavior_grants",
                "reviewed request digest does not bind the exact behavior request",
            ));
        }
        ensure_behavior_within_app_resources(
            "requested_behavior_grants",
            request,
            requested_background,
            requested_app_resources,
        )?;
    }

    let mut granted_ids = HashSet::new();
    let mut previous_granted_id: Option<&AppName> = None;
    for grant in granted {
        validate_behavior_grant_shape("granted_behavior_grants", grant)?;
        if previous_granted_id.is_some_and(|previous| previous >= &grant.behavior_id) {
            return Err(AppContractError::invalid(
                "granted_behavior_grants",
                "is duplicated or not in canonical behavior-id order",
            ));
        }
        previous_granted_id = Some(&grant.behavior_id);
        if !granted_ids.insert(grant.behavior_id.clone()) {
            return Err(AppContractError::invalid(
                "granted_behavior_grants",
                "contains duplicate behavior ids",
            ));
        }
        let request = requested_by_id.get(&grant.behavior_id).ok_or_else(|| {
            AppContractError::invalid(
                "granted_behavior_grants",
                "contains a behavior that was not requested",
            )
        })?;
        if grant.purpose != request.purpose
            || grant.action != request.action
            || grant.input_selector_digest != request.input_selector_digest
            || grant.operations != request.operations
            || grant.steps_digest != request.steps_digest
            || grant.output_schema_digest != request.output_schema_digest
            || grant.reviewed_request_digest != request.reviewed_request_digest
            || grant.min_interval_seconds < request.min_interval_seconds
            || !behavior_resources_narrow(&grant.resources, &request.resources)
        {
            return Err(AppContractError::invalid(
                "granted_behavior_grants",
                "widens or substitutes the exact reviewed behavior request",
            ));
        }
        ensure_behavior_within_app_resources(
            "granted_behavior_grants",
            grant,
            granted_background,
            granted_app_resources,
        )?;
    }
    Ok(())
}

fn validate_behavior_grant_shape(
    field: &'static str,
    grant: &AppBehaviorGrant,
) -> Result<(), AppContractError> {
    if grant.purpose.trim().is_empty()
        || grant.purpose.len() > APP_BEHAVIOR_MAX_PURPOSE_BYTES
        || grant.purpose.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(AppContractError::invalid(
            field,
            "purpose must contain bounded, non-control review text",
        ));
    }
    if grant.operations.len() > APP_BEHAVIOR_MAX_OPERATIONS {
        return Err(AppContractError::invalid(
            field,
            "operations exceed the behavior operation ceiling",
        ));
    }
    let unique_operations = grant.operations.iter().collect::<HashSet<_>>();
    if unique_operations.len() != grant.operations.len() {
        return Err(AppContractError::invalid(
            field,
            "operations contains duplicates",
        ));
    }
    if grant.operations.is_empty() && grant.output_schema_digest.is_some() {
        return Err(AppContractError::invalid(
            field,
            "model output schema requires at least one reviewed behavior operation",
        ));
    }
    if !(APP_BEHAVIOR_MIN_INTERVAL_SECONDS..=APP_BEHAVIOR_MAX_INTERVAL_SECONDS)
        .contains(&grant.min_interval_seconds)
        || grant.resources.max_tokens_per_run == 0
        || grant.resources.max_cost_microusd_per_run == 0
        || grant.resources.max_active_seconds_per_run == 0
        || grant.resources.max_tokens_per_month == 0
        || grant.resources.max_cost_microusd_per_month == 0
        || grant.resources.max_starts_per_period == 0
        || grant.resources.max_starts_per_period > APP_BEHAVIOR_MAX_STARTS_PER_PERIOD
        || grant.resources.period_seconds > APP_BEHAVIOR_MAX_PERIOD_SECONDS
        || grant.resources.max_causation_depth == 0
        || grant.resources.max_causation_depth > APP_BEHAVIOR_MAX_CAUSATION_DEPTH
        || grant.resources.max_spend_depth == 0
        || grant.resources.max_spend_depth > APP_BEHAVIOR_MAX_SPEND_DEPTH
        || grant.resources.max_contribution_proposals_per_run
            > APP_BEHAVIOR_MAX_CONTRIBUTION_PROPOSALS_PER_RUN
        || grant.resources.max_tokens_per_run > grant.resources.max_tokens_per_month
        || grant.resources.max_cost_microusd_per_run > grant.resources.max_cost_microusd_per_month
    {
        return Err(AppContractError::invalid(
            field,
            "contains an invalid cadence or resource ceiling",
        ));
    }
    Ok(())
}

fn behavior_resources_narrow(
    grant: &AppBehaviorResourceCeiling,
    request: &AppBehaviorResourceCeiling,
) -> bool {
    grant.period_seconds == request.period_seconds
        && grant.max_tokens_per_run <= request.max_tokens_per_run
        && grant.max_cost_microusd_per_run <= request.max_cost_microusd_per_run
        && grant.max_active_seconds_per_run <= request.max_active_seconds_per_run
        && grant.max_tokens_per_month <= request.max_tokens_per_month
        && grant.max_cost_microusd_per_month <= request.max_cost_microusd_per_month
        && grant.max_starts_per_period <= request.max_starts_per_period
        && grant.max_causation_depth <= request.max_causation_depth
        && grant.max_spend_depth <= request.max_spend_depth
        && grant.max_contribution_proposals_per_run <= request.max_contribution_proposals_per_run
}

fn ensure_behavior_within_app_resources(
    field: &'static str,
    behavior: &AppBehaviorGrant,
    background: &AppBackgroundExecution,
    app: &AppResourceCeiling,
) -> Result<(), AppContractError> {
    let background_interval = match background {
        AppBackgroundExecution::Granted {
            min_interval_seconds,
            ..
        } => *min_interval_seconds,
        AppBackgroundExecution::Denied => {
            return Err(AppContractError::invalid(
                field,
                "cannot grant a behavior while background execution is denied",
            ));
        },
    };
    if behavior.min_interval_seconds < background_interval
        || behavior.resources.max_tokens_per_run
            > app.max_input_tokens.saturating_add(app.max_output_tokens)
        || behavior.resources.max_cost_microusd_per_run > app.max_cost_microusd
        || behavior.resources.max_active_seconds_per_run > app.max_active_seconds
        || behavior.resources.max_tokens_per_month > app.max_monthly_tokens
        || behavior.resources.max_cost_microusd_per_month > app.max_monthly_cost_microusd
    {
        return Err(AppContractError::invalid(
            field,
            "behavior cadence or resources widen the app-level authority ceiling",
        ));
    }
    Ok(())
}

fn validate_event_behavior_grants(
    requested: &[AppEventBehaviorGrant],
    granted: &[AppEventBehaviorGrant],
    requested_background: &AppBackgroundExecution,
    granted_background: &AppBackgroundExecution,
    requested_app_resources: &AppResourceCeiling,
    granted_app_resources: &AppResourceCeiling,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    if requested.len() > APP_EVENT_BEHAVIOR_MAX_DECLARATIONS
        || granted.len() > APP_EVENT_BEHAVIOR_MAX_DECLARATIONS
        || requested.len() > limits.max_collection_items()
        || granted.len() > limits.max_collection_items()
    {
        return Err(AppContractError::invalid(
            "event_behavior_grants",
            "exceeds the collection ceiling",
        ));
    }
    let mut requested_by_id = HashMap::new();
    let mut previous_requested_id: Option<&AppName> = None;
    for request in requested {
        validate_event_behavior_grant_shape("requested_event_behavior_grants", request)?;
        if request.resources.period_seconds < request.min_interval_seconds
            || u64::from(request.resources.max_starts_per_period)
                > request
                    .resources
                    .period_seconds
                    .div_ceil(request.min_interval_seconds)
        {
            return Err(AppContractError::invalid(
                "requested_event_behavior_grants",
                "requested frequency period or cap is inconsistent with the event interval",
            ));
        }
        if previous_requested_id.is_some_and(|previous| previous >= &request.event_behavior_id) {
            return Err(AppContractError::invalid(
                "requested_event_behavior_grants",
                "is duplicated or not in canonical event-behavior-id order",
            ));
        }
        previous_requested_id = Some(&request.event_behavior_id);
        if requested_by_id
            .insert(request.event_behavior_id.clone(), request)
            .is_some()
        {
            return Err(AppContractError::invalid(
                "requested_event_behavior_grants",
                "contains duplicate event behavior ids",
            ));
        }
        let expected_subscription_digest = app_event_subscription_digest(&request.subscription)?;
        let expected_projection_schema_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value(event_behavior_projection_schema(&request.subscription))
                .map_err(|error| {
                    AppContractError::invalid("requested_event_behavior_grants", error.to_string())
                })?,
        )
        .map_err(|error| {
            AppContractError::invalid("requested_event_behavior_grants", error.to_string())
        })?;
        let expected_request_digest = app_event_behavior_request_digest(
            &request.event_behavior_id,
            &request.purpose,
            &request.action,
            &request.subscription_digest,
            &request.projection_schema_digest,
            &request.operations,
            request.steps_digest.as_ref(),
            request.output_schema_digest.as_ref(),
            request.min_interval_seconds,
            &request.resources,
        )?;
        if request.subscription_digest != expected_subscription_digest
            || request.projection_schema_digest != expected_projection_schema_digest
            || request.reviewed_request_digest != expected_request_digest
        {
            return Err(AppContractError::invalid(
                "requested_event_behavior_grants",
                "reviewed digests do not bind the exact subscription/projection request",
            ));
        }
        ensure_event_behavior_within_app_resources(
            "requested_event_behavior_grants",
            request,
            requested_background,
            requested_app_resources,
        )?;
    }

    let mut previous_granted_id: Option<&AppName> = None;
    for grant in granted {
        validate_event_behavior_grant_shape("granted_event_behavior_grants", grant)?;
        if previous_granted_id.is_some_and(|previous| previous >= &grant.event_behavior_id) {
            return Err(AppContractError::invalid(
                "granted_event_behavior_grants",
                "is duplicated or not in canonical event-behavior-id order",
            ));
        }
        previous_granted_id = Some(&grant.event_behavior_id);
        let request = requested_by_id
            .get(&grant.event_behavior_id)
            .ok_or_else(|| {
                AppContractError::invalid(
                    "granted_event_behavior_grants",
                    "contains an event behavior that was not requested",
                )
            })?;
        if grant.purpose != request.purpose
            || grant.action != request.action
            || grant.subscription != request.subscription
            || grant.subscription_digest != request.subscription_digest
            || grant.projection_schema_digest != request.projection_schema_digest
            || grant.operations != request.operations
            || grant.steps_digest != request.steps_digest
            || grant.output_schema_digest != request.output_schema_digest
            || grant.reviewed_request_digest != request.reviewed_request_digest
            || grant.min_interval_seconds < request.min_interval_seconds
            || !behavior_resources_narrow(&grant.resources, &request.resources)
        {
            return Err(AppContractError::invalid(
                "granted_event_behavior_grants",
                "widens or substitutes the exact reviewed event behavior request",
            ));
        }
        ensure_event_behavior_within_app_resources(
            "granted_event_behavior_grants",
            grant,
            granted_background,
            granted_app_resources,
        )?;
    }
    Ok(())
}

fn validate_event_behavior_grant_shape(
    field: &'static str,
    grant: &AppEventBehaviorGrant,
) -> Result<(), AppContractError> {
    if grant.purpose.trim().is_empty()
        || grant.purpose.len() > APP_BEHAVIOR_MAX_PURPOSE_BYTES
        || grant.purpose.bytes().any(|byte| byte.is_ascii_control())
        || grant.operations.len() > APP_BEHAVIOR_MAX_OPERATIONS
        || grant.operations.iter().collect::<HashSet<_>>().len() != grant.operations.len()
        || (grant.operations.is_empty() && grant.output_schema_digest.is_some())
    {
        return Err(AppContractError::invalid(
            field,
            "contains invalid purpose, operation, or output-schema authority",
        ));
    }
    let AppEventSubscriptionV1::InstallationExecutionTerminal { outcomes } = &grant.subscription;
    if outcomes.is_empty()
        || outcomes.len() > 3
        || outcomes.iter().collect::<HashSet<_>>().len() != outcomes.len()
        || outcomes.windows(2).any(|pair| pair[0] >= pair[1])
        || outcomes.contains(&AppEventTerminalOutcomeV1::Cancelled)
    {
        return Err(AppContractError::invalid(
            field,
            "contains an empty, duplicate, or over-broad terminal outcome filter",
        ));
    }
    if !(APP_BEHAVIOR_MIN_INTERVAL_SECONDS..=APP_BEHAVIOR_MAX_INTERVAL_SECONDS)
        .contains(&grant.min_interval_seconds)
    {
        return Err(AppContractError::invalid(
            field,
            "contains an invalid event cadence or start cap",
        ));
    }
    validate_behavior_resource_shape(field, &grant.resources)
}

fn validate_behavior_resource_shape(
    field: &'static str,
    resources: &AppBehaviorResourceCeiling,
) -> Result<(), AppContractError> {
    if resources.max_tokens_per_run == 0
        || resources.max_cost_microusd_per_run == 0
        || resources.max_active_seconds_per_run == 0
        || resources.max_tokens_per_month == 0
        || resources.max_cost_microusd_per_month == 0
        || resources.max_starts_per_period == 0
        || resources.max_starts_per_period > APP_BEHAVIOR_MAX_STARTS_PER_PERIOD
        || resources.period_seconds > APP_BEHAVIOR_MAX_PERIOD_SECONDS
        || resources.max_causation_depth == 0
        || resources.max_causation_depth > APP_BEHAVIOR_MAX_CAUSATION_DEPTH
        || resources.max_spend_depth == 0
        || resources.max_spend_depth > APP_BEHAVIOR_MAX_SPEND_DEPTH
        || resources.max_contribution_proposals_per_run
            > APP_BEHAVIOR_MAX_CONTRIBUTION_PROPOSALS_PER_RUN
        || resources.max_tokens_per_run > resources.max_tokens_per_month
        || resources.max_cost_microusd_per_run > resources.max_cost_microusd_per_month
    {
        return Err(AppContractError::invalid(
            field,
            "contains an invalid resource ceiling",
        ));
    }
    Ok(())
}

fn ensure_event_behavior_within_app_resources(
    field: &'static str,
    behavior: &AppEventBehaviorGrant,
    background: &AppBackgroundExecution,
    app: &AppResourceCeiling,
) -> Result<(), AppContractError> {
    let background_interval = match background {
        AppBackgroundExecution::Granted {
            min_interval_seconds,
            ..
        } => *min_interval_seconds,
        AppBackgroundExecution::Denied => {
            return Err(AppContractError::invalid(
                field,
                "cannot grant an event behavior while background execution is denied",
            ));
        },
    };
    if behavior.min_interval_seconds < background_interval
        || behavior.resources.max_tokens_per_run
            > app.max_input_tokens.saturating_add(app.max_output_tokens)
        || behavior.resources.max_cost_microusd_per_run > app.max_cost_microusd
        || behavior.resources.max_active_seconds_per_run > app.max_active_seconds
        || behavior.resources.max_tokens_per_month > app.max_monthly_tokens
        || behavior.resources.max_cost_microusd_per_month > app.max_monthly_cost_microusd
    {
        return Err(AppContractError::invalid(
            field,
            "event behavior cadence or resources widen app-level authority",
        ));
    }
    Ok(())
}

fn validate_notification_grants(
    requested: &[AppNotificationGrant],
    granted: &[AppNotificationGrant],
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    if requested.len() > APP_NOTIFICATION_MAX_TOTAL_PORTS
        || granted.len() > APP_NOTIFICATION_MAX_TOTAL_PORTS
        || requested.len() > limits.max_collection_items()
        || granted.len() > limits.max_collection_items()
    {
        return Err(AppContractError::invalid(
            "notification_grants",
            "exceeds the collection ceiling",
        ));
    }
    let mut requested_by_key = HashMap::new();
    let mut previous_requested_key: Option<(&AppName, &AppName)> = None;
    for request in requested {
        validate_notification_grant_shape("requested_notification_grants", request)?;
        let key = (&request.workflow_id, &request.port_id);
        if previous_requested_key.is_some_and(|previous| previous >= key) {
            return Err(AppContractError::invalid(
                "requested_notification_grants",
                "is duplicated or not in canonical workflow/port order",
            ));
        }
        previous_requested_key = Some(key);
        if requested_by_key
            .insert(
                (request.workflow_id.clone(), request.port_id.clone()),
                request,
            )
            .is_some()
        {
            return Err(AppContractError::invalid(
                "requested_notification_grants",
                "contains duplicate workflow/port identities",
            ));
        }
        let expected_digest = app_notification_request_digest(
            &request.workflow_id,
            &request.port_id,
            &request.purpose,
            request.kind,
            request.severity_ceiling,
            request.max_notifications_per_period,
            request.period_seconds,
            request.max_pending,
            request.ttl_seconds,
        )?;
        if expected_digest != request.reviewed_request_digest {
            return Err(AppContractError::invalid(
                "requested_notification_grants",
                "reviewed request digest does not bind the exact notification port",
            ));
        }
    }

    let mut previous_granted_key: Option<(&AppName, &AppName)> = None;
    for grant in granted {
        validate_notification_grant_shape("granted_notification_grants", grant)?;
        let key = (&grant.workflow_id, &grant.port_id);
        if previous_granted_key.is_some_and(|previous| previous >= key) {
            return Err(AppContractError::invalid(
                "granted_notification_grants",
                "is duplicated or not in canonical workflow/port order",
            ));
        }
        previous_granted_key = Some(key);
        let request = requested_by_key
            .get(&(grant.workflow_id.clone(), grant.port_id.clone()))
            .ok_or_else(|| {
                AppContractError::invalid(
                    "granted_notification_grants",
                    "contains a notification port that was not requested",
                )
            })?;
        if grant.purpose != request.purpose
            || grant.kind != request.kind
            || grant.period_seconds != request.period_seconds
            || grant.reviewed_request_digest != request.reviewed_request_digest
            || grant.severity_ceiling.rank() > request.severity_ceiling.rank()
            || grant.max_notifications_per_period > request.max_notifications_per_period
            || grant.max_pending > request.max_pending
            || grant.ttl_seconds > request.ttl_seconds
        {
            return Err(AppContractError::invalid(
                "granted_notification_grants",
                "widens or substitutes the exact reviewed notification port",
            ));
        }
    }
    Ok(())
}

fn validate_notification_grant_shape(
    field: &'static str,
    grant: &AppNotificationGrant,
) -> Result<(), AppContractError> {
    if grant.purpose.trim().is_empty()
        || grant.purpose.len() > APP_NOTIFICATION_MAX_PURPOSE_BYTES
        || grant.purpose.bytes().any(|byte| byte.is_ascii_control())
        || grant.max_notifications_per_period == 0
        || grant.max_notifications_per_period > APP_NOTIFICATION_MAX_PER_PERIOD
        || u64::from(grant.max_notifications_per_period)
            > grant
                .period_seconds
                .div_ceil(APP_NOTIFICATION_MIN_PERIOD_SECONDS)
        || !(APP_NOTIFICATION_MIN_PERIOD_SECONDS..=APP_NOTIFICATION_MAX_PERIOD_SECONDS)
            .contains(&grant.period_seconds)
        || grant.max_pending == 0
        || grant.max_pending > APP_NOTIFICATION_MAX_PENDING
        || !(APP_NOTIFICATION_MIN_TTL_SECONDS..=APP_NOTIFICATION_MAX_TTL_SECONDS)
            .contains(&grant.ttl_seconds)
        || grant.severity_ceiling == AppNotificationSeverityV1::Critical
    {
        return Err(AppContractError::invalid(
            field,
            "contains an invalid notification kind, severity, purpose, or volume ceiling",
        ));
    }
    Ok(())
}

/// Granted custom-surface entry points keep the manifest kernel's shape —
/// at most `APP_CUSTOM_SURFACE_MAX_ENTRY_POINTS`, canonical bounded routes,
/// `surfaces/`-relative `.html` documents — and each `(route, document)`
/// pair at most once. Order is the reviewed declaration order (it is what
/// the approve receipt echoes), so uniqueness is checked by key set, not
/// by ordering.
fn validate_granted_custom_surface_entry_points(
    entries: &[AppGrantedCustomSurfaceEntryPoint],
    _limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    if entries.len() > APP_CUSTOM_SURFACE_MAX_ENTRY_POINTS {
        return Err(AppContractError::invalid(
            "granted_custom_surface_entry_points",
            "exceeds the custom-surface entry-point ceiling",
        ));
    }
    let mut seen = HashSet::new();
    for entry in entries {
        if entry.route.is_empty()
            || entry.route.len() > 192
            || !entry.route.starts_with('/')
            || entry
                .route
                .bytes()
                .any(|byte| !byte.is_ascii_graphic() || byte == b',')
        {
            return Err(AppContractError::invalid(
                "granted_custom_surface_entry_points",
                "contains a route that is not a canonical bounded route",
            ));
        }
        if entry.document.is_empty()
            || entry.document.len() > 512
            || !entry.document.starts_with("surfaces/")
            || !entry.document.ends_with(".html")
            || entry.document.contains("..")
            || entry.document.contains('\\')
        {
            return Err(AppContractError::invalid(
                "granted_custom_surface_entry_points",
                "contains a document that is not a surfaces/ HTML member path",
            ));
        }
        if !seen.insert((entry.route.as_str(), entry.document.as_str())) {
            return Err(AppContractError::invalid(
                "granted_custom_surface_entry_points",
                "grants one entry point more than once",
            ));
        }
    }
    Ok(())
}

impl ValidateAppContract for AppSchemaRevision {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_policy(&self.canonical_data_handling_policy, limits)?;
        let mut nodes = 0usize;
        let mut bytes = 0usize;
        for value in [
            &self.canonical_entity_schema,
            &self.compiled_validation_schema,
            &self.compiled_index_plan,
        ] {
            let (value_nodes, value_bytes) = validate_json_value(value, limits)?;
            nodes = nodes.saturating_add(value_nodes);
            bytes = bytes.saturating_add(value_bytes);
        }
        if nodes > limits.max_value_nodes() || bytes > limits.max_value_bytes() {
            return Err(AppContractError::invalid(
                "schema",
                "compiled schema members exceed the aggregate value ceiling",
            ));
        }
        if self.compatibility_with_previous == AppSchemaCompatibility::MigrationRequired
            && self.migration_plan_ref.is_none()
        {
            return Err(AppContractError::invalid(
                "migration_plan_ref",
                "is required when compatibility requires migration",
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppRecordRevision {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.dataset_generation == 0 {
            return Err(AppContractError::invalid(
                "dataset_generation",
                "must be greater than zero",
            ));
        }
        if !self.payload.is_object() {
            return Err(AppContractError::invalid(
                "payload",
                "entity record payloads must be JSON objects",
            ));
        }
        validate_json_value(&self.payload, limits)?;
        if let Some(policy) = &self.handling_override {
            validate_policy(policy, limits)?;
        }
        ensure_time_order("updated_at", &self.created_at, &self.updated_at)?;
        if let Some(deleted_at) = self.deleted_at.as_ref() {
            ensure_time_order("deleted_at", &self.updated_at, deleted_at)?;
        }
        validate_unique_refs(
            "source_artifact_refs",
            &self.provenance.source_artifact_refs,
            limits,
        )?;
        validate_unique_refs("citation_refs", &self.provenance.citation_refs, limits)?;
        validate_record_provenance(&self.provenance)?;
        Ok(())
    }
}

impl ValidateAppContract for AppRunBinding {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if let Some(identity) = self.behavior_resource_identity.as_ref() {
            identity.validate_app_contract(limits)?;
        }
        Ok(())
    }
}

impl ValidateAppContract for AppMutationReceipt {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_nonempty_bounded(
            "committed_record_revisions",
            self.committed_record_revisions.len(),
            limits.max_collection_items(),
        )?;
        let mut committed = HashSet::with_capacity(self.committed_record_revisions.len());
        for record in &self.committed_record_revisions {
            if !committed.insert((&record.entity, &record.record_id, record.revision)) {
                return Err(AppContractError::invalid(
                    "committed_record_revisions",
                    "contains a duplicate committed revision",
                ));
            }
        }
        if self.change_seq_range.first == 0
            || self.change_seq_range.last < self.change_seq_range.first
        {
            return Err(AppContractError::invalid(
                "change_seq_range",
                "must be a positive ordered range",
            ));
        }
        if matches!(
            &self.origin,
            AppMutationOrigin::Migration {
                migration_batch: 0,
                ..
            }
        ) {
            return Err(AppContractError::invalid(
                "origin.migration_batch",
                "must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppSurfaceBinding {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_route("app_local_route", &self.app_local_route, false)?;
        validate_route("canonical_host_route", &self.canonical_host_route, true)?;
        let expected_prefix = format!("/apps/{}/", self.installation_id.as_str());
        if self.canonical_host_route != expected_prefix.trim_end_matches('/')
            && !self.canonical_host_route.starts_with(&expected_prefix)
        {
            return Err(AppContractError::invalid(
                "canonical_host_route",
                "must be namespaced beneath the exact installation route",
            ));
        }
        let expected_route = if self.app_local_route == "/" {
            expected_prefix.trim_end_matches('/').to_owned()
        } else {
            format!(
                "{}{}",
                expected_prefix.trim_end_matches('/'),
                self.app_local_route
            )
        };
        if self.canonical_host_route != expected_route {
            return Err(AppContractError::invalid(
                "canonical_host_route",
                "must exactly match the installation namespace plus app_local_route",
            ));
        }
        Ok(())
    }
}

impl ValidateAppContract for AppDisclosureEnvelope {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_bounded(
            "approved_projections",
            self.approved_projections.len(),
            limits.max_collection_items(),
        )?;
        let mut entities = HashSet::with_capacity(self.approved_projections.len());
        for projection in &self.approved_projections {
            if !entities.insert(&projection.entity) {
                return Err(AppContractError::invalid(
                    "approved_projections",
                    "contains a duplicate entity projection",
                ));
            }
            validate_nonempty_bounded(
                "approved_projection.fields",
                projection.fields.len(),
                limits.max_collection_items(),
            )?;
            let mut fields = HashSet::with_capacity(projection.fields.len());
            if projection.fields.iter().any(|field| !fields.insert(field)) {
                return Err(AppContractError::invalid(
                    "approved_projection.fields",
                    "contains duplicate fields",
                ));
            }
            validate_unique_refs(
                "approved_projection.record_revisions",
                &projection.record_revisions,
                limits,
            )?;
        }
        match self.provider_class {
            AppDisclosureProviderClass::Deterministic if self.provider_endpoint_ref.is_some() => {
                return Err(AppContractError::invalid(
                    "provider_endpoint_ref",
                    "deterministic processing cannot name a model/tool endpoint",
                ));
            },
            AppDisclosureProviderClass::LocalModel
            | AppDisclosureProviderClass::RemoteModel
            | AppDisclosureProviderClass::ExternalTool
                if self.provider_endpoint_ref.is_none() =>
            {
                return Err(AppContractError::invalid(
                    "provider_endpoint_ref",
                    "is required for model and external-tool processing",
                ));
            },
            _ => {},
        }
        if self.provider_class == AppDisclosureProviderClass::ExternalTool
            && self.destination.is_none()
        {
            return Err(AppContractError::invalid(
                "destination",
                "is required for external-tool disclosure",
            ));
        }
        if self.max_rows == 0
            || self.max_bytes == 0
            || self.max_tokens == 0
            || self.max_nodes == 0
            || self.max_relation_depth == 0
        {
            return Err(AppContractError::invalid(
                "disclosure_limits",
                "all disclosure ceilings must be greater than zero",
            ));
        }
        ensure_time_after("expires_at", &self.issued_at, &self.expires_at)
    }
}

impl ValidateAppContract for AppInstallationApproval {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.workflow_material_bindings.len() > limits.max_collection_items() {
            return Err(AppContractError::invalid(
                "workflow_material_bindings",
                "exceeds the collection ceiling",
            ));
        }
        let mut workflows = HashSet::new();
        for binding in &self.workflow_material_bindings {
            let personality_complete = matches!(
                (
                    &binding.personality_ref,
                    &binding.personality_content_digest,
                    &binding.personality_descriptor_digest,
                ),
                (Some(_), Some(_), Some(_)) | (None, None, None)
            );
            if binding.schema != "magician.app-reviewed-workflow-material.v1"
                || !binding.agent_ref.as_str().starts_with("agent:")
                || binding
                    .personality_ref
                    .as_ref()
                    .is_some_and(|reference| !reference.as_str().starts_with("personality:"))
                || !personality_complete
                || !workflows.insert(binding.workflow_id.as_str())
            {
                return Err(AppContractError::invalid(
                    "workflow_material_bindings",
                    "contains invalid identity evidence or a duplicate workflow",
                ));
            }
        }
        validate_reviewed_contribution_port_grants(
            &self.contribution_grants,
            limits.max_collection_items(),
        )?;
        validate_interactive_capability_grants(
            "interactive_capability_grants",
            &self.interactive_capability_grants,
            limits,
        )?;
        ensure_time_after("expires_at", &self.issued_at, &self.expires_at)?;
        match (
            self.consumed_at.as_ref(),
            self.consumed_installation_revision,
        ) {
            (Some(consumed_at), Some(_)) => {
                ensure_time_order("consumed_at", &self.issued_at, consumed_at)?;
                if consumed_at >= &self.expires_at {
                    return Err(AppContractError::invalid(
                        "consumed_at",
                        "must be earlier than expires_at",
                    ));
                }
            },
            (None, None) => {},
            _ => {
                return Err(AppContractError::invalid(
                    "consumed_at",
                    "consumed_at and consumed_installation_revision must be present together",
                ));
            },
        }
        Ok(())
    }
}

fn parse_version_requirement(field: &'static str, value: &str) -> Result<(), AppContractError> {
    if value.is_empty() || value.len() > 64 {
        return Err(AppContractError::invalid(
            field,
            "must contain between 1 and 64 bytes",
        ));
    }
    semver::VersionReq::parse(value)
        .map(|_| ())
        .map_err(|error| AppContractError::invalid(field, error.to_string()))
}

fn parse_exact_version_label(field: &'static str, value: &str) -> Result<(), AppContractError> {
    if value.is_empty() || value.len() > 64 {
        return Err(AppContractError::invalid(
            field,
            "must contain between 1 and 64 bytes",
        ));
    }
    let mut segments = 0usize;
    for segment in value.split('.') {
        segments = segments.saturating_add(1);
        if segment.is_empty()
            || !segment.bytes().all(|byte| byte.is_ascii_digit())
            || (segment.len() > 1 && segment.starts_with('0'))
        {
            return Err(AppContractError::invalid(
                field,
                "must be an exact numeric version label, not a range or wildcard",
            ));
        }
    }
    if segments > 3 {
        return Err(AppContractError::invalid(
            field,
            "must contain at most three numeric components",
        ));
    }
    Ok(())
}

fn validate_unique_refs(
    field: &'static str,
    values: &[AppReference],
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    validate_bounded(field, values.len(), limits.max_collection_items())?;
    let mut unique = HashSet::with_capacity(values.len());
    if values.iter().any(|value| !unique.insert(value)) {
        return Err(AppContractError::invalid(field, "contains duplicates"));
    }
    Ok(())
}

fn ensure_ref_subset(
    field: &'static str,
    granted: &[AppReference],
    requested: &[AppReference],
) -> Result<(), AppContractError> {
    let requested: HashSet<_> = requested.iter().collect();
    if granted.iter().any(|value| !requested.contains(value)) {
        return Err(AppContractError::invalid(
            field,
            "contains authority that was not requested",
        ));
    }
    Ok(())
}

fn validate_projection_grants(
    field: &'static str,
    grants: &[AppEntityProjectionGrant],
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    validate_bounded(field, grants.len(), limits.max_collection_items())?;
    let mut entities = HashSet::with_capacity(grants.len());
    for grant in grants {
        if !entities.insert(&grant.entity) {
            return Err(AppContractError::invalid(
                field,
                "contains a duplicate entity",
            ));
        }
        validate_nonempty_bounded(field, grant.fields.len(), limits.max_collection_items())?;
        let mut fields = HashSet::with_capacity(grant.fields.len());
        if grant.fields.iter().any(|value| !fields.insert(value)) {
            return Err(AppContractError::invalid(
                field,
                "contains duplicate fields",
            ));
        }
    }
    Ok(())
}

fn ensure_projection_subset(
    granted: &[AppEntityProjectionGrant],
    requested: &[AppEntityProjectionGrant],
) -> Result<(), AppContractError> {
    let requested: HashMap<_, _> = requested
        .iter()
        .map(|grant| (&grant.entity, grant))
        .collect();
    for grant in granted {
        let Some(requested) = requested.get(&grant.entity) else {
            return Err(AppContractError::invalid(
                "granted_personal_agent_data_access",
                "contains an entity that was not requested",
            ));
        };
        let requested_fields: HashSet<_> = requested.fields.iter().collect();
        if grant
            .fields
            .iter()
            .any(|field| !requested_fields.contains(field))
            || (grant.search && !requested.search)
        {
            return Err(AppContractError::invalid(
                "granted_personal_agent_data_access",
                "broadens requested fields or search access",
            ));
        }
    }
    Ok(())
}

pub fn validate_policy(
    policy: &AppDataHandlingPolicy,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    validate_unique_refs(
        "approved_destinations",
        &policy.approved_destinations,
        limits,
    )?;
    match policy.external_egress {
        AppExternalEgress::Denied if !policy.approved_destinations.is_empty() => {
            Err(AppContractError::invalid(
                "approved_destinations",
                "must be empty when external egress is denied",
            ))
        },
        AppExternalEgress::ApprovedDestinations if policy.approved_destinations.is_empty() => {
            Err(AppContractError::invalid(
                "approved_destinations",
                "must not be empty when destination egress is approved",
            ))
        },
        _ => Ok(()),
    }
}

fn validate_record_provenance(provenance: &AppRecordProvenance) -> Result<(), AppContractError> {
    if provenance.output_revision.is_some() != provenance.execution_id.is_some() {
        return Err(AppContractError::invalid(
            "provenance",
            "execution_id and output_revision must be present together",
        ));
    }
    match provenance.actor_kind {
        AppRecordActorKind::Workflow
            if provenance.execution_id.is_none() || provenance.mutation_receipt_id.is_none() =>
        {
            Err(AppContractError::invalid(
                "provenance",
                "workflow records require exact execution/output and mutation receipt identity",
            ))
        },
        AppRecordActorKind::Surface | AppRecordActorKind::Migration
            if provenance.mutation_receipt_id.is_none() =>
        {
            Err(AppContractError::invalid(
                "provenance.mutation_receipt_id",
                "surface and migration records require a mutation receipt",
            ))
        },
        _ => Ok(()),
    }
}

fn ensure_policy_narrows(
    granted: &AppDataHandlingPolicy,
    requested: &AppDataHandlingPolicy,
) -> Result<(), AppContractError> {
    if granted.classification_floor < requested.classification_floor
        || granted.model_processing > requested.model_processing
        || granted.personal_agent_access > requested.personal_agent_access
        || granted.memory_promotion > requested.memory_promotion
        || granted.external_egress > requested.external_egress
    {
        return Err(AppContractError::invalid(
            "granted_data_handling_policy",
            "broadens the requested policy",
        ));
    }
    ensure_ref_subset(
        "granted_data_handling_policy.approved_destinations",
        &granted.approved_destinations,
        &requested.approved_destinations,
    )
}

fn ensure_background_narrows(
    granted: &AppBackgroundExecution,
    requested: &AppBackgroundExecution,
) -> Result<(), AppContractError> {
    match (granted, requested) {
        (AppBackgroundExecution::Denied, _) => Ok(()),
        (AppBackgroundExecution::Granted { .. }, AppBackgroundExecution::Denied) => {
            Err(AppContractError::invalid(
                "granted_background_execution",
                "cannot grant background execution when it was not requested",
            ))
        },
        (
            AppBackgroundExecution::Granted {
                min_interval_seconds: granted_interval,
                max_concurrent_runs: granted_concurrency,
            },
            AppBackgroundExecution::Granted {
                min_interval_seconds: requested_interval,
                max_concurrent_runs: requested_concurrency,
            },
        ) if *granted_interval >= *requested_interval
            && *granted_concurrency > 0
            && *granted_concurrency <= *requested_concurrency =>
        {
            Ok(())
        },
        _ => Err(AppContractError::invalid(
            "granted_background_execution",
            "broadens the requested interval or concurrency",
        )),
    }
}

fn validate_background_execution(
    field: &'static str,
    execution: &AppBackgroundExecution,
) -> Result<(), AppContractError> {
    if let AppBackgroundExecution::Granted {
        min_interval_seconds,
        max_concurrent_runs,
    } = execution
    {
        if *min_interval_seconds == 0 || *max_concurrent_runs == 0 {
            return Err(AppContractError::invalid(
                field,
                "granted background execution requires a positive interval and concurrency",
            ));
        }
    }
    Ok(())
}

fn ensure_background_within_resource_ceiling(
    field: &'static str,
    execution: &AppBackgroundExecution,
    ceiling: &AppResourceCeiling,
) -> Result<(), AppContractError> {
    if let AppBackgroundExecution::Granted {
        max_concurrent_runs,
        ..
    } = execution
    {
        if *max_concurrent_runs > ceiling.max_concurrent_background_runs {
            return Err(AppContractError::invalid(
                field,
                "background concurrency exceeds its resource ceiling",
            ));
        }
    }
    Ok(())
}

fn validate_network_policy(
    policy: &AppNetworkPolicy,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    if let AppNetworkPolicy::ApprovedDestinations { destinations } = policy {
        validate_nonempty_bounded(
            "network_policy.destinations",
            destinations.len(),
            limits.max_collection_items(),
        )?;
        validate_unique_refs("network_policy.destinations", destinations, limits)?;
    }
    Ok(())
}

fn ensure_network_narrows(
    granted: &AppNetworkPolicy,
    requested: &AppNetworkPolicy,
) -> Result<(), AppContractError> {
    match (granted, requested) {
        (AppNetworkPolicy::Denied, _) => Ok(()),
        (
            AppNetworkPolicy::ApprovedDestinations {
                destinations: granted,
            },
            AppNetworkPolicy::ApprovedDestinations {
                destinations: requested,
            },
        ) => ensure_ref_subset("granted_network_policy", granted, requested),
        _ => Err(AppContractError::invalid(
            "granted_network_policy",
            "grants destinations when network access was denied",
        )),
    }
}

fn validate_resource_ceiling(
    field: &'static str,
    ceiling: &AppResourceCeiling,
) -> Result<(), AppContractError> {
    // Zero is a real fail-closed ceiling: it denies that resource. It must not
    // be reinterpreted as "unlimited" by a later accounting adapter.
    let per_run_tokens = ceiling
        .max_input_tokens
        .checked_add(ceiling.max_output_tokens)
        .ok_or_else(|| {
            AppContractError::invalid(field, "per-run token ceilings overflow their unit")
        })?;
    if ceiling.max_active_seconds > ceiling.max_lifetime_seconds
        || ceiling.max_cost_microusd > ceiling.max_monthly_cost_microusd
        || per_run_tokens > ceiling.max_monthly_tokens
    {
        return Err(AppContractError::invalid(
            field,
            "per-run ceilings cannot exceed their lifetime/monthly ceilings",
        ));
    }
    Ok(())
}

/// Validate every independently supplied authority ceiling before it can
/// participate in an intersection. Authority resolution accepts these values
/// from trusted registries rather than the app wire contract, so it must apply
/// the same bounded invariants explicitly at the final resolution seam.
pub fn validate_authority_ceiling_parts(
    tools_len: usize,
    context_reads_len: usize,
    policy: &AppDataHandlingPolicy,
    background: &AppBackgroundExecution,
    network: &AppNetworkPolicy,
    resources: &AppResourceCeiling,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    validate_bounded(
        "authority_ceiling.tools",
        tools_len,
        limits.max_collection_items(),
    )?;
    validate_bounded(
        "authority_ceiling.context_reads",
        context_reads_len,
        limits.max_collection_items(),
    )?;
    validate_policy(policy, limits)?;
    validate_background_execution("authority_ceiling.background_execution", background)?;
    validate_network_policy(network, limits)?;
    validate_resource_ceiling("authority_ceiling.resources", resources)?;
    ensure_background_within_resource_ceiling(
        "authority_ceiling.background_execution",
        background,
        resources,
    )
}

fn ensure_resource_narrows(
    granted: &AppResourceCeiling,
    requested: &AppResourceCeiling,
) -> Result<(), AppContractError> {
    let granted_values = resource_values(granted);
    let requested_values = resource_values(requested);
    if granted_values
        .iter()
        .zip(requested_values)
        .any(|(granted, requested)| *granted > requested)
    {
        return Err(AppContractError::invalid(
            "granted_resource_ceiling",
            "contains a ceiling larger than requested",
        ));
    }
    Ok(())
}

fn resource_values(ceiling: &AppResourceCeiling) -> [u64; 14] {
    [
        ceiling.max_input_tokens,
        ceiling.max_output_tokens,
        ceiling.max_cost_microusd,
        ceiling.max_paid_tool_invocations,
        ceiling.max_active_seconds,
        ceiling.max_lifetime_seconds,
        ceiling.max_browser_network_actions,
        u64::from(ceiling.max_concurrent_foreground_runs),
        u64::from(ceiling.max_concurrent_background_runs),
        ceiling.max_records,
        ceiling.max_payload_bytes,
        ceiling.max_attachment_bytes,
        ceiling.max_monthly_tokens,
        ceiling.max_monthly_cost_microusd,
    ]
}

fn validate_route(
    field: &'static str,
    route: &str,
    allow_apps_prefix: bool,
) -> Result<(), AppContractError> {
    if route.is_empty() || route.len() > 512 || !route.is_ascii() || !route.starts_with('/') {
        return Err(AppContractError::invalid(
            field,
            "must be an absolute portable-ASCII route no longer than 512 bytes",
        ));
    }
    if route.contains('?')
        || route.contains('#')
        || route.contains('%')
        || route.contains('\\')
        || route.contains("//")
        || route.chars().any(char::is_control)
    {
        return Err(AppContractError::invalid(
            field,
            "contains traversal, query, fragment, control, or backslash syntax",
        ));
    }
    if route == "/" {
        return Ok(());
    }
    let segments = route.strip_prefix('/').unwrap_or_default().split('/');
    let mut segment_count = 0usize;
    let mut first_segment = None;
    let mut parameters = HashSet::new();
    for segment in segments {
        segment_count = segment_count.saturating_add(1);
        if first_segment.is_none() {
            first_segment = Some(segment);
        }
        if segment.is_empty() || matches!(segment, "." | "..") {
            return Err(AppContractError::invalid(
                field,
                "contains empty or traversal segments",
            ));
        }
        if let Some(parameter) = segment.strip_prefix(':') {
            AppName::parse(parameter).map_err(|error| {
                AppContractError::invalid(field, format!("invalid route parameter: {error}"))
            })?;
            if !parameters.insert(parameter.to_ascii_lowercase()) {
                return Err(AppContractError::invalid(
                    field,
                    "contains duplicate route parameters",
                ));
            }
        } else if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(AppContractError::invalid(
                field,
                "contains unsupported route-segment characters",
            ));
        }
    }
    if segment_count > 32 {
        return Err(AppContractError::invalid(
            field,
            "contains more than 32 route segments",
        ));
    }
    if !allow_apps_prefix
        && first_segment.is_some_and(|segment| segment.eq_ignore_ascii_case("apps"))
    {
        return Err(AppContractError::invalid(
            field,
            "app-local routes cannot claim the host app namespace",
        ));
    }
    Ok(())
}

fn ensure_time_order(
    field: &'static str,
    earlier: &DateTime<Utc>,
    later: &DateTime<Utc>,
) -> Result<(), AppContractError> {
    if later < earlier {
        return Err(AppContractError::invalid(
            field,
            "cannot precede its earlier lifecycle timestamp",
        ));
    }
    Ok(())
}

fn ensure_time_after(
    field: &'static str,
    earlier: &DateTime<Utc>,
    later: &DateTime<Utc>,
) -> Result<(), AppContractError> {
    if later <= earlier {
        return Err(AppContractError::invalid(
            field,
            "must be later than its issue timestamp",
        ));
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::apps::{
        lifecycle::AppStableOperationalStatus, models::decode_app_contract,
    };

    fn timestamp(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 14, 0, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn digest(value: &str) -> AppDigest {
        AppDigest::blake3(value.as_bytes())
    }

    fn installation_id() -> AppInstallationId {
        AppInstallationId::parse("install_1").unwrap()
    }

    /// A recipe change must change the reviewed digest.
    ///
    /// Without this the allow-set, action, selector and schemas could stay
    /// byte-identical while a package update reordered the steps or dropped a
    /// guard — and the scheduler would keep dispatching against the old grant.
    /// The recipe IS review material, so it has to be inside the seal.
    #[test]
    fn behavior_review_digest_binds_the_ordered_recipe() {
        let behavior_id = AppName::parse("ambient_turn").unwrap();
        let action = AppName::parse("take_turn").unwrap();
        let selector = digest("selector");
        let resources = AppBehaviorResourceCeiling {
            max_tokens_per_run: 100,
            max_cost_microusd_per_run: 100,
            max_active_seconds_per_run: 10,
            max_tokens_per_month: 1_000,
            max_cost_microusd_per_month: 1_000,
            max_starts_per_period: 24,
            period_seconds: 86_400,
            max_causation_depth: 1,
            max_spend_depth: 1,
            max_contribution_proposals_per_run: 0,
        };
        let operations = vec![
            AppName::parse("engagement_gate").unwrap(),
            AppName::parse("compose_post").unwrap(),
        ];
        let one_recipe = digest("gate-then-compose");
        let other_recipe = digest("compose-then-gate");

        let with_one = app_behavior_request_digest(
            &behavior_id,
            "Give one agent a turn.",
            &action,
            &selector,
            &operations,
            Some(&one_recipe),
            None,
            900,
            &resources,
        )
        .unwrap();
        let with_other = app_behavior_request_digest(
            &behavior_id,
            "Give one agent a turn.",
            &action,
            &selector,
            &operations,
            Some(&other_recipe),
            None,
            900,
            &resources,
        )
        .unwrap();
        assert_ne!(
            with_one, with_other,
            "reordering the recipe must not preserve the reviewed digest"
        );

        // And a behavior with no recipe keeps the digest it had before this
        // contract existed, so adding the field did not invalidate installed
        // grants that never had steps.
        let without = app_behavior_request_digest(
            &behavior_id,
            "Give one agent a turn.",
            &action,
            &selector,
            &operations,
            None,
            None,
            900,
            &resources,
        )
        .unwrap();
        assert_ne!(without, with_one);
    }

    #[test]
    fn behavior_review_digest_binds_exact_owner_visible_purpose() {
        let behavior_id = AppName::parse("daily_summary").unwrap();
        let action = AppName::parse("summarize").unwrap();
        let selector = digest("selector");
        let resources = AppBehaviorResourceCeiling {
            max_tokens_per_run: 100,
            max_cost_microusd_per_run: 100,
            max_active_seconds_per_run: 10,
            max_tokens_per_month: 1_000,
            max_cost_microusd_per_month: 1_000,
            max_starts_per_period: 24,
            period_seconds: 86_400,
            max_causation_depth: 1,
            max_spend_depth: 1,
            max_contribution_proposals_per_run: 0,
        };
        let first = app_behavior_request_digest(
            &behavior_id,
            "Summarize the selected record.",
            &action,
            &selector,
            &[],
            None,
            None,
            3_600,
            &resources,
        )
        .unwrap();
        let substituted = app_behavior_request_digest(
            &behavior_id,
            "Delete the selected record.",
            &action,
            &selector,
            &[],
            None,
            None,
            3_600,
            &resources,
        )
        .unwrap();
        assert_ne!(first, substituted);

        let requested = AppBehaviorGrant {
            behavior_id: behavior_id.clone(),
            purpose: "Summarize the selected record.".to_owned(),
            action: action.clone(),
            input_selector_digest: selector,
            operations: Vec::new(),
            steps_digest: None,
            output_schema_digest: None,
            min_interval_seconds: 3_600,
            resources: resources.clone(),
            reviewed_request_digest: first,
        };
        let mut slowed = requested.clone();
        slowed.min_interval_seconds = 172_800;
        validate_behavior_grants(
            std::slice::from_ref(&requested),
            std::slice::from_ref(&slowed),
            &AppBackgroundExecution::Granted {
                min_interval_seconds: 3_600,
                max_concurrent_runs: 1,
            },
            &AppBackgroundExecution::Granted {
                min_interval_seconds: 3_600,
                max_concurrent_runs: 1,
            },
            &ceiling(1_000),
            &ceiling(1_000),
            &AppContractLimits::default(),
        )
        .expect("owner cadence narrowing may cross the fixed frequency period");
    }

    fn item4_event_grant(resources: AppBehaviorResourceCeiling) -> AppEventBehaviorGrant {
        let subscription = AppEventSubscriptionV1::InstallationExecutionTerminal {
            outcomes: vec![AppEventTerminalOutcomeV1::Succeeded],
        };
        let subscription_digest = app_event_subscription_digest(&subscription).unwrap();
        let projection_schema_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value(event_behavior_projection_schema(&subscription)).unwrap(),
        )
        .unwrap();
        let event_behavior_id = AppName::parse("on_completion").unwrap();
        let action = AppName::parse("summarize_completion").unwrap();
        let purpose = "Summarize one canonical completion.".to_owned();
        let min_interval_seconds = 120;
        let reviewed_request_digest = app_event_behavior_request_digest(
            &event_behavior_id,
            &purpose,
            &action,
            &subscription_digest,
            &projection_schema_digest,
            &[],
            None,
            None,
            min_interval_seconds,
            &resources,
        )
        .unwrap();
        AppEventBehaviorGrant {
            event_behavior_id,
            purpose,
            action,
            subscription,
            subscription_digest,
            projection_schema_digest,
            steps_digest: None,
            operations: Vec::new(),
            output_schema_digest: None,
            min_interval_seconds,
            resources,
            reviewed_request_digest,
        }
    }

    fn item4_notification_grant() -> AppNotificationGrant {
        let workflow_id = AppName::parse("daily_digest").unwrap();
        let port_id = AppName::parse("owner_briefing").unwrap();
        let purpose = "Keep the owner informed.".to_owned();
        let kind = AppNotificationKindV1::Briefing;
        let severity_ceiling = AppNotificationSeverityV1::Warning;
        let max_notifications_per_period = 4;
        let period_seconds = 3_600;
        let max_pending = 4;
        let ttl_seconds = 7_200;
        let reviewed_request_digest = app_notification_request_digest(
            &workflow_id,
            &port_id,
            &purpose,
            kind,
            severity_ceiling,
            max_notifications_per_period,
            period_seconds,
            max_pending,
            ttl_seconds,
        )
        .unwrap();
        AppNotificationGrant {
            workflow_id,
            port_id,
            purpose,
            kind,
            severity_ceiling,
            max_notifications_per_period,
            period_seconds,
            max_pending,
            ttl_seconds,
            reviewed_request_digest,
        }
    }

    #[test]
    fn item4_grant_validation_binds_exact_requests_and_only_allows_narrowing() {
        let requested_resources = AppBehaviorResourceCeiling {
            max_tokens_per_run: 20,
            max_cost_microusd_per_run: 10,
            max_active_seconds_per_run: 10,
            max_tokens_per_month: 20,
            max_cost_microusd_per_month: 10,
            max_starts_per_period: 4,
            period_seconds: 3_600,
            max_causation_depth: 2,
            max_spend_depth: 2,
            max_contribution_proposals_per_run: 1,
        };
        let requested_event = item4_event_grant(requested_resources);
        let mut granted_event = requested_event.clone();
        granted_event.min_interval_seconds = 240;
        granted_event.resources.max_tokens_per_run = 10;
        granted_event.resources.max_cost_microusd_per_run = 5;
        granted_event.resources.max_active_seconds_per_run = 5;
        granted_event.resources.max_tokens_per_month = 10;
        granted_event.resources.max_cost_microusd_per_month = 5;
        granted_event.resources.max_starts_per_period = 2;
        granted_event.resources.max_causation_depth = 1;
        granted_event.resources.max_spend_depth = 1;
        granted_event.resources.max_contribution_proposals_per_run = 0;

        let requested_notification = item4_notification_grant();
        let mut granted_notification = requested_notification.clone();
        granted_notification.severity_ceiling = AppNotificationSeverityV1::Info;
        granted_notification.max_notifications_per_period = 2;
        granted_notification.max_pending = 2;
        granted_notification.ttl_seconds = 3_600;

        let legacy_authority_digest = app_granted_authority_digest(&grant(), &[]).unwrap();
        let mut event_authority = grant();
        event_authority.granted_event_behavior_grants = vec![requested_event.clone()];
        let event_authority_digest = app_granted_authority_digest(&event_authority, &[]).unwrap();
        assert_ne!(legacy_authority_digest, event_authority_digest);
        let mut notification_authority = grant();
        notification_authority.granted_notification_grants = vec![requested_notification.clone()];
        let notification_authority_digest =
            app_granted_authority_digest(&notification_authority, &[]).unwrap();
        assert_ne!(legacy_authority_digest, notification_authority_digest);
        assert_ne!(event_authority_digest, notification_authority_digest);

        let mut valid = grant();
        valid.requested_event_behavior_grants = vec![requested_event.clone()];
        valid.granted_event_behavior_grants = vec![granted_event];
        valid.requested_notification_grants = vec![requested_notification.clone()];
        valid.granted_notification_grants = vec![granted_notification];
        valid
            .validate_app_contract(&AppContractLimits::default())
            .expect("exact item-4 requests may be narrowed by the owner");

        let mut substituted_projection = valid.clone();
        substituted_projection.requested_event_behavior_grants[0].subscription =
            AppEventSubscriptionV1::InstallationExecutionTerminal {
                outcomes: vec![AppEventTerminalOutcomeV1::Failed],
            };
        assert!(substituted_projection
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("digests do not bind"));

        let mut widened_notification = valid;
        widened_notification.granted_notification_grants[0].ttl_seconds =
            requested_notification.ttl_seconds + 1;
        assert!(widened_notification
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("widens or substitutes"));
    }

    fn policy(processing: AppModelProcessing) -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: processing,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion: AppMemoryPromotion::CandidateAllowed,
            external_egress: AppExternalEgress::ApprovedDestinations,
            approved_destinations: vec![reference("provider:video")],
        }
    }

    fn ceiling(value: u64) -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: value,
            max_output_tokens: value,
            max_cost_microusd: value,
            max_paid_tool_invocations: value,
            max_active_seconds: value,
            max_lifetime_seconds: value,
            max_browser_network_actions: value,
            max_concurrent_foreground_runs: u16::try_from(value).unwrap(),
            max_concurrent_background_runs: u16::try_from(value).unwrap(),
            max_records: value,
            max_payload_bytes: value,
            max_attachment_bytes: value,
            max_monthly_tokens: value.saturating_mul(2),
            max_monthly_cost_microusd: value,
        }
    }

    fn grant() -> AppGrantRevision {
        let projection = AppEntityProjectionGrant {
            entity: AppName::parse("clip").unwrap(),
            fields: vec![AppFieldPath::parse("title").unwrap()],
            search: true,
        };
        AppGrantRevision {
            installation_id: installation_id(),
            revision: AppRevision::new(1).unwrap(),
            package_revision_ref: reference("package:1"),
            requested_tools: vec![reference("video:render")],
            granted_tools: vec![reference("video:render")],
            requested_agents: Vec::new(),
            granted_agents: Vec::new(),
            requested_personalities: Vec::new(),
            granted_personalities: Vec::new(),
            requested_interactive_capabilities: Vec::new(),
            granted_interactive_capabilities: Vec::new(),
            granted_custom_surface_entry_points: Vec::new(),
            requested_behavior_grants: Vec::new(),
            granted_behavior_grants: Vec::new(),
            requested_event_behavior_grants: Vec::new(),
            granted_event_behavior_grants: Vec::new(),
            requested_notification_grants: Vec::new(),
            granted_notification_grants: Vec::new(),
            requested_memory_read: None,
            granted_memory_read: None,
            requested_secret_uses: None,
            granted_secret_uses: None,
            granted_any_public_host: false,
            requested_context_reads: vec![reference("context:owner")],
            granted_context_reads: vec![reference("context:owner")],
            requested_personal_agent_data_access: vec![projection.clone()],
            granted_personal_agent_data_access: vec![projection],
            requested_data_handling_policy: policy(AppModelProcessing::RemoteAllowed),
            granted_data_handling_policy: policy(AppModelProcessing::LocalOnly),
            granted_data_handling_policy_digest: digest("policy:1"),
            requested_background_execution: AppBackgroundExecution::Granted {
                min_interval_seconds: 60,
                max_concurrent_runs: 2,
            },
            granted_background_execution: AppBackgroundExecution::Granted {
                min_interval_seconds: 120,
                max_concurrent_runs: 1,
            },
            requested_network_policy: AppNetworkPolicy::ApprovedDestinations {
                destinations: vec![reference("provider:video")],
            },
            granted_network_policy: AppNetworkPolicy::ApprovedDestinations {
                destinations: vec![reference("provider:video")],
            },
            requested_resource_ceiling: ceiling(10),
            granted_resource_ceiling: ceiling(5),
            approved_by: reference("actor:owner"),
            approved_at: timestamp(1),
            authority_digest: digest("authority:1"),
            revoked_at: None,
        }
    }

    #[test]
    fn grant_validation_proves_every_axis_only_narrows() {
        let limits = AppContractLimits::default();
        grant().validate_app_contract(&limits).unwrap();

        let mut broadened = grant();
        broadened.granted_tools.push(reference("invented:tool"));
        assert!(broadened
            .validate_app_contract(&limits)
            .unwrap_err()
            .to_string()
            .contains("not requested"));

        let mut remote = grant();
        remote.granted_data_handling_policy.model_processing = AppModelProcessing::RemoteAllowed;
        remote.requested_data_handling_policy.model_processing = AppModelProcessing::LocalOnly;
        assert!(remote.validate_app_contract(&limits).is_err());

        let mut expensive = grant();
        expensive.granted_resource_ceiling.max_cost_microusd = 11;
        assert!(expensive.validate_app_contract(&limits).is_err());

        let mut invented_agent = grant();
        invented_agent
            .granted_agents
            .push(reference("agent:research-agent"));
        assert!(invented_agent
            .validate_app_contract(&limits)
            .unwrap_err()
            .to_string()
            .contains("not requested"));
    }

    #[test]
    fn installation_cannot_claim_enabled_before_all_active_revisions_exist() {
        let installation = AppInstallation {
            scope: AppScope {
                principal: reference("anonymous"),
                workspace: reference("default"),
            },
            installation_id: installation_id(),
            package_revision_ref: reference("package:1"),
            lifecycle: AppInstallationLifecycle {
                status: AppInstallationStatus::Enabled,
                generation: 2,
                update_return_status: None,
            },
            grant_revision: Some(AppRevision::new(1).unwrap()),
            active_schema_revision: None,
            active_surface_revision: Some(AppRevision::new(1).unwrap()),
            created_at: timestamp(0),
            updated_at: timestamp(1),
            disabled_at: None,
            quarantined_at: None,
            uninstalled_at: None,
            purged_at: None,
        };
        let error = installation
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err();
        assert!(error.to_string().contains("post-review states require"));
    }

    #[test]
    fn review_state_rejects_even_one_premature_active_revision() {
        let installation = AppInstallation {
            scope: AppScope {
                principal: reference("anonymous"),
                workspace: reference("default"),
            },
            installation_id: installation_id(),
            package_revision_ref: reference("package:1"),
            lifecycle: AppInstallationLifecycle::ready_for_review(),
            grant_revision: Some(AppRevision::new(1).unwrap()),
            active_schema_revision: None,
            active_surface_revision: None,
            created_at: timestamp(0),
            updated_at: timestamp(1),
            disabled_at: None,
            quarantined_at: None,
            uninstalled_at: None,
            purged_at: None,
        };
        assert!(installation
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("cannot expose active"));
    }

    #[test]
    fn zero_resource_ceiling_is_a_valid_denial_not_unlimited() {
        let mut denied = grant();
        denied.requested_background_execution = AppBackgroundExecution::Denied;
        denied.granted_background_execution = AppBackgroundExecution::Denied;
        denied.requested_resource_ceiling = ceiling(0);
        denied.granted_resource_ceiling = ceiling(0);
        denied
            .validate_app_contract(&AppContractLimits::default())
            .unwrap();
    }

    #[test]
    fn one_run_cannot_request_more_tokens_than_its_monthly_ceiling() {
        let mut invalid = grant();
        invalid.requested_resource_ceiling.max_monthly_tokens = 19;
        assert!(invalid
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("monthly"));
    }

    #[test]
    fn background_grant_requires_positive_values_within_resource_ceiling() {
        let mut invalid = grant();
        invalid.granted_background_execution = AppBackgroundExecution::Granted {
            min_interval_seconds: 0,
            max_concurrent_runs: 1,
        };
        assert!(invalid
            .validate_app_contract(&AppContractLimits::default())
            .is_err());

        let mut over_ceiling = grant();
        over_ceiling
            .granted_resource_ceiling
            .max_concurrent_background_runs = 0;
        assert!(over_ceiling
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("resource ceiling"));
    }

    #[test]
    fn attempt_failure_is_explicit_and_cannot_rebind_initial_install() {
        let attempt = AppLifecycleAttempt {
            attempt_id: reference("attempt:1"),
            kind: AppLifecycleAttemptKind::InitialInstall,
            installation_id: Some(installation_id()),
            source_installation_generation: Some(1),
            candidate_package_revision_ref: reference("package:1"),
            state: AppLifecycleAttemptState::Failed,
            conformance_attestation_ref: None,
            permission_migration_diff_ref: None,
            approval_ref: None,
            failure_code: Some(AppName::parse("invalid_manifest").unwrap()),
            created_at: timestamp(0),
            updated_at: timestamp(1),
        };
        assert!(attempt
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("initial_install"));
    }

    #[test]
    fn record_payload_is_depth_admitted_before_deserialization_on_a_small_stack() {
        let body = json!({
            "installation_id": "install_1",
            "entity_name": "clip",
            "record_id": "record_1",
            "record_revision": 1,
            "dataset_generation": 1,
            "schema_revision": 1,
            "payload": [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[0]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]],
            "created_at": "2026-08-14T00:00:00Z",
            "updated_at": "2026-08-14T00:00:01Z",
            "provenance": {
                "actor_kind": "user",
                "actor_id": "actor:owner"
            }
        });
        let encoded = serde_json::to_vec(&body).unwrap();
        let result = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                decode_app_contract::<AppRecordRevision>(&encoded, &AppContractLimits::default())
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(matches!(
            result,
            Err(AppContractError::JsonDepthExceeded { .. })
        ));
    }

    #[test]
    fn mutation_receipt_origin_and_sequence_are_structurally_exact() {
        let receipt = AppMutationReceipt {
            receipt_id: reference("receipt:1"),
            installation_id: installation_id(),
            origin: AppMutationOrigin::Migration {
                migration_run_id: reference("migration:1"),
                migration_batch: 0,
            },
            mutation_key: digest("mutation:1"),
            batch_digest: digest("batch:1"),
            committed_record_revisions: vec![AppCommittedRecordRevision {
                entity: AppName::parse("clip").unwrap(),
                record_id: AppRecordId::parse("record_1").unwrap(),
                revision: AppRevision::new(1).unwrap(),
            }],
            change_seq_range: AppChangeSequenceRange { first: 2, last: 1 },
            committed_at: timestamp(1),
        };
        assert!(receipt
            .validate_app_contract(&AppContractLimits::default())
            .is_err());
    }

    #[test]
    fn disclosure_requires_exact_endpoint_destination_and_expiry() {
        let disclosure = AppDisclosureEnvelope {
            disclosure_id: reference("disclosure:1"),
            scope: AppScope {
                principal: reference("anonymous"),
                workspace: reference("default"),
            },
            installation_id: installation_id(),
            execution_id: reference("execution:1"),
            purpose: AppName::parse("render").unwrap(),
            provider_class: AppDisclosureProviderClass::ExternalTool,
            provider_endpoint_ref: Some(reference("endpoint:higgsfield")),
            package_revision_ref: reference("package:1"),
            grant_revision: AppRevision::new(1).unwrap(),
            schema_revision: AppRevision::new(1).unwrap(),
            approved_projections: vec![AppApprovedRecordProjection {
                entity: AppName::parse("clip").unwrap(),
                fields: vec![AppFieldPath::parse("title").unwrap()],
                record_revisions: Vec::new(),
            }],
            destination: None,
            max_rows: 1,
            max_bytes: 1,
            max_tokens: 1,
            max_nodes: 1,
            max_relation_depth: 1,
            redaction_policy_digest: digest("redaction:1"),
            content_projection_digest: digest("projection:1"),
            issued_at: timestamp(1),
            expires_at: timestamp(2),
        };
        assert!(disclosure
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("destination"));

        let mut direct_input_disclosure = disclosure;
        direct_input_disclosure.destination = Some(reference("destination:approved"));
        direct_input_disclosure.approved_projections.clear();
        direct_input_disclosure
            .validate_app_contract(&AppContractLimits::default())
            .expect("direct input can disclose no stored record projection");
    }

    #[test]
    fn approval_is_single_use_as_one_paired_consumption_record() {
        let mut approval = AppInstallationApproval {
            approval_id: reference("approval:1"),
            revision: AppRevision::new(1).unwrap(),
            authenticated_scope_ref: AppScopeBindingRef::parse("scope_1").unwrap(),
            actor_ref: reference("actor:owner"),
            session_ref: reference("session:1"),
            authentication: AppApprovalAuthentication::AuthenticatedSession,
            authentication_revision: AppRevision::new(1).unwrap(),
            install_or_update_attempt_id: reference("attempt:1"),
            package_content_digest: digest("package:1"),
            requested_authority_digest: digest("requested:1"),
            granted_authority_digest: digest("granted:1"),
            data_policy_diff_digest: digest("data:1"),
            resource_diff_digest: digest("resource:1"),
            schema_diff_digest: digest("schema:1"),
            migration_diff_digest: digest("migration:1"),
            global_policy_revision: AppRevision::new(1).unwrap(),
            workflow_material_bindings: Vec::new(),
            contribution_grants: Vec::new(),
            interactive_capability_grants: Vec::new(),
            issued_at: timestamp(1),
            expires_at: timestamp(3),
            consumed_at: Some(timestamp(2)),
            consumed_installation_revision: None,
        };
        assert!(approval
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("present together"));

        approval.consumed_installation_revision = Some(AppRevision::new(2).unwrap());
        approval
            .workflow_material_bindings
            .push(AppReviewedWorkflowMaterialBinding {
                schema: "magician.app-reviewed-workflow-material.v1".to_owned(),
                workflow_id: AppName::parse("run").unwrap(),
                agent_ref: reference("agent:reviewed"),
                agent_definition_revision: AppRevision::new(1).unwrap(),
                agent_definition_digest: digest("agent-definition"),
                agent_descriptor_digest: digest("agent-descriptor"),
                personality_ref: Some(reference("personality:precise")),
                personality_content_digest: None,
                personality_descriptor_digest: None,
                binding_digest: digest("workflow-material"),
            });
        assert!(approval
            .validate_app_contract(&AppContractLimits::default())
            .unwrap_err()
            .to_string()
            .contains("identity evidence"));
    }

    #[test]
    fn update_return_state_is_not_encoded_in_attempt_authority() {
        let lifecycle = AppInstallationLifecycle {
            status: AppInstallationStatus::UpdatePending,
            generation: 3,
            update_return_status: Some(AppStableOperationalStatus::Enabled),
        };
        lifecycle.validate().unwrap();
    }

    #[test]
    fn package_schema_and_sdk_versions_are_exact_labels_not_requirements() {
        for accepted in ["1", "1.0", "1.2.3"] {
            parse_exact_version_label("version", accepted).expect("exact version label");
        }
        for rejected in ["*", ">=1", "^1", "1.x", "01", "1.02", "1.2.3.4"] {
            assert!(
                parse_exact_version_label("version", rejected).is_err(),
                "{rejected}"
            );
        }
    }

    #[test]
    fn surface_routes_reject_encoded_unicode_and_case_folded_namespace_escapes() {
        for route in [
            "/safe/%2e%2e/escape",
            "/safe/％32e",
            "/safe/＼escape",
            "/safe/é",
            "/APPS/claimed",
        ] {
            assert!(
                validate_route("app_local_route", route, false).is_err(),
                "unsafe app route accepted: {route}"
            );
        }
        validate_route("app_local_route", "/clips/:clip_id", false)
            .expect("canonical app-local route");

        let binding = AppSurfaceBinding {
            installation_id: installation_id(),
            surface_revision: AppRevision::new(1).unwrap(),
            package_revision_ref: reference("package:1"),
            app_local_route: "/clips/:clip_id".to_owned(),
            canonical_host_route: "/apps/install_1/clips/:clip_id".to_owned(),
            view_id: AppName::parse("clips").unwrap(),
            compiled_view_digest: digest("view:1"),
            published_surface_ref: None,
            status: AppSurfaceStatus::Active,
        };
        binding
            .validate_app_contract(&AppContractLimits::default())
            .expect("namespaced canonical route");
        let mut mismatched = binding;
        mismatched.canonical_host_route = "/apps/install_1/settings".to_owned();
        assert!(mismatched
            .validate_app_contract(&AppContractLimits::default())
            .is_err());
    }

    fn navigation_entry(id: &str, route: &str, title: &str) -> AppManifestNavigationEntry {
        use crate::magician_v2::apps::manifest::{
            AppManifestNavigationPlacement, AppManifestNavigationSurface, AppRoute,
        };
        AppManifestNavigationEntry {
            id: AppName::parse(id).unwrap(),
            title: title.to_owned(),
            route: AppRoute::parse(route).unwrap(),
            placement: AppManifestNavigationPlacement::Section {
                section: AppName::parse("observe").unwrap(),
            },
            surface: AppManifestNavigationSurface::View {
                view: AppName::parse("sessions").unwrap(),
            },
        }
    }

    fn directory_metadata(
        navigation: Vec<AppManifestNavigationEntry>,
    ) -> AppPackageDirectoryMetadata {
        AppPackageDirectoryMetadata {
            package_revision_ref: reference("package:1"),
            name: AppName::parse("meetings").unwrap(),
            description: "Meetings".to_owned(),
            actions: Vec::new(),
            custom_surface_entry_count: 0,
            navigation,
            manifest_digest: digest("manifest:1"),
            created_at: timestamp(1),
        }
    }

    /// Directory metadata is what the shell reads; the manifest is not.
    ///
    /// A client refuses a whole directory entry whose navigation is off shape
    /// rather than mounting half a list, so an unbounded or self-colliding
    /// declaration reaching the store would make the app disappear from the
    /// directory instead of merely losing a link. The absent case matters
    /// separately: metadata persisted before this field existed must decode
    /// unchanged and re-encode to the same canonical bytes, or every
    /// already-admitted package trips the admission byte-identity fence.
    #[test]
    fn directory_metadata_carries_navigation_and_fails_closed_on_off_shape() {
        let limits = AppContractLimits::default();

        let declared = directory_metadata(vec![navigation_entry(
            "meetings_console",
            "/meetings-console",
            "Meetings",
        )]);
        declared
            .validate_app_contract(&limits)
            .expect("one bounded declaration validates");
        let encoded = serde_json::to_vec(&declared).unwrap();
        let decoded: AppPackageDirectoryMetadata = decode_app_contract(&encoded, &limits).unwrap();
        assert_eq!(decoded, declared);

        let absent = directory_metadata(Vec::new());
        let wire = serde_json::to_value(&absent).expect("metadata serializes");
        assert!(
            wire.get("navigation").is_none(),
            "a package that declares none must keep its pre-field canonical bytes"
        );
        decode_app_contract::<AppPackageDirectoryMetadata>(
            &serde_json::to_vec(&json!({
                "package_revision_ref": "package:1",
                "name": "meetings",
                "description": "Meetings",
                "actions": [],
                "manifest_digest": digest("manifest:1").to_string(),
                "created_at": "2026-08-14T00:00:01Z",
            }))
            .unwrap(),
            &limits,
        )
        .expect("metadata persisted before the field decodes unchanged");

        for refused in [
            vec![
                navigation_entry("meetings_console", "/one", "Meetings"),
                navigation_entry("meetings_console", "/two", "Meetings"),
            ],
            vec![
                navigation_entry("one", "/meetings-console", "Meetings"),
                navigation_entry("two", "/meetings-console", "Meetings"),
            ],
            vec![navigation_entry("meetings_console", "/one", "   ")],
            vec![navigation_entry(
                "meetings_console",
                "/one",
                &"m".repeat(APP_SURFACING_MAX_TITLE_BYTES + 1),
            )],
            (0..=APP_NAVIGATION_MAX_ENTRIES)
                .map(|index| navigation_entry(&format!("nav_{index}"), &format!("/n{index}"), "M"))
                .collect(),
        ] {
            assert!(
                directory_metadata(refused)
                    .validate_app_contract(&limits)
                    .is_err(),
                "off-shape navigation must be refused, not stored"
            );
        }
    }
}
