//! Strict app-package manifest and complete-bundle admission kernel.
//!
//! This module intentionally does not walk a filesystem, publish packages or
//! route app manifests through the live skill loader. The Phase-1B no-follow
//! staging reader supplies its inventory of members; this pure kernel rejects
//! unsafe paths, links, special files, normalization collisions, incomplete
//! references and hostile YAML before computing an immutable bundle identity.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Read,
};

pub use magician_app_contract::{
    contribution::AppContributionEvidenceClass, llm_operations::AppManifestLlmOperation,
    AppManifestDistribution, AppManifestFeature, AppManifestGeneratedBy, AppManifestPermission,
    APP_AUTHORING_SDK_SEMVER, APP_AUTHORING_SDK_VERSION, APP_MANIFEST_SCHEMA_VERSION,
};
use magician_app_contract::{
    contribution::{
        APP_CONTRIBUTION_MAX_AUDIENCES, APP_CONTRIBUTION_MAX_SELECTED_FIELDS,
        APP_CONTRIBUTION_MAX_TTL_MS,
    },
    llm_operations::APP_LLM_OPERATION_MAX_PURPOSE_BYTES,
};
use serde::{
    de::{self, Deserializer},
    Deserialize, Serialize,
};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use super::{
    interactive::AppInteractiveCapabilityRequest,
    models::{
        AppContractLimits, AppDataClassification, AppDigest, AppFieldPath, AppModelProcessing,
        AppName, AppRecordId, AppReference,
    },
    recipe_ir::{
        compile_workflow_value_schema, validate_json_workflow_value_shape,
        AppCompiledWorkflowValueSchema, AppWorkflowHandlingFloor, AppWorkflowRecordField,
        AppWorkflowValueSchemaSource, AppWorkflowValueSchemaVersion, AppWorkflowValueTypeNode,
    },
    records::{
        AppContributionDestination, AppContributionDestinationBinding, AppContributionFrequency,
        AppContributionSource, AppEventSubscriptionV1, AppEventTerminalOutcomeV1,
        AppExternalEgress, AppMemoryPromotion, AppNotificationKindV1, AppNotificationSeverityV1,
        AppPersonalAgentAccess, APP_CONTRIBUTION_MAX_EVIDENCE_CLASSES,
        APP_CONTRIBUTION_MAX_PORTS_PER_WORKFLOW, APP_CONTRIBUTION_MAX_PURPOSES,
    },
};
use crate::magician_v2::json_traversal::canonical_json_bytes;

const ABSOLUTE_MAX_BUNDLE_PATH_BYTES: usize = 512;
const MAX_ROUTE_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppPackageLimits {
    max_manifest_bytes: usize,
    max_yaml_depth: usize,
    max_yaml_nodes: usize,
    max_string_bytes: usize,
    max_entities: usize,
    max_fields: usize,
    max_views: usize,
    max_workflows: usize,
    max_actions: usize,
    max_assets: usize,
    max_dependencies: usize,
    max_bundle_files: usize,
    max_bundle_file_bytes: usize,
    max_bundle_bytes: usize,
    max_bundle_path_bytes: usize,
}

#[cfg(test)]
mod background_behavior_contract_source_oracles {
    #[test]
    fn singleton_selector_and_grant_digest_stay_source_pinned() {
        let manifest_source = include_str!("manifest.rs");
        assert!(manifest_source.contains("pub input: AppManifestBehaviorInputSelector"));
        assert!(manifest_source.contains("pub record_id: AppRecordId"));
        assert!(manifest_source.contains("validate_behavior_input_selector(behavior"));
        assert!(manifest_source.contains("projected_source.nodes != workflow_source.nodes"));

        let records_source = include_str!("records.rs");
        assert!(records_source.contains("pub purpose: String"));
        assert!(records_source.contains("\"purpose\": purpose"));
        assert!(records_source.contains("grant.purpose != request.purpose"));
        assert!(records_source.contains("pub input_selector_digest: AppDigest"));
        assert!(records_source.contains("\"input_selector_digest\": input_selector_digest"));
        assert!(
            records_source.contains("grant.input_selector_digest != request.input_selector_digest")
        );
    }

    #[test]
    fn deterministic_behaviors_may_omit_llm_operations_but_not_claim_model_output() {
        let manifest_source = include_str!("manifest.rs");
        assert!(manifest_source
            .contains("behavior.operations.is_empty() && behavior.output_schema.is_some()"));
        assert!(!manifest_source
            .contains("validate_nonempty_unique_values(\n            \"behavior.operations\""));

        let records_source = include_str!("records.rs");
        assert!(records_source
            .contains("grant.operations.is_empty() && grant.output_schema_digest.is_some()"));
    }

    #[test]
    fn event_and_notification_contracts_stay_separate_exact_and_fail_closed() {
        let manifest_source = include_str!("manifest.rs");
        assert!(manifest_source.contains("pub event_behaviors: Vec<AppManifestEventBehavior>"));
        assert!(manifest_source
            .contains("pub notification_ports: BTreeMap<AppName, AppManifestNotificationPort>"));
        assert!(
            manifest_source.contains("workflow input must exactly match its host event projection")
        );
        assert!(
            manifest_source.contains("outcomes.contains(&AppEventTerminalOutcomeV1::Cancelled)")
        );
        assert!(manifest_source
            .contains("port.severity_ceiling == AppNotificationSeverityV1::Critical"));

        let records_source = include_str!("records.rs");
        assert!(records_source.contains("magician.app-event-behavior-review.v1"));
        assert!(records_source.contains("magician.app-owner-notification-review.v1"));
        assert!(records_source
            .contains("grant.projection_schema_digest != request.projection_schema_digest"));
        assert!(records_source.contains("granted_event_behavior_grants.is_empty()"));
        assert!(records_source.contains("granted_notification_grants.is_empty()"));

        let review_source =
            include_str!("../../../../magician-apps/src/apps/installation_review.rs");
        assert!(review_source
            .contains("let Some(selected) = selected else {\n        return Ok(Vec::new());"));
        assert!(review_source.contains("requested_event_projection_schemas"));
    }
}

/// Prose ceiling for one string inside an agent definition. Large enough for a
/// persona, small enough that a definition cannot become a data channel.
pub(crate) const MAX_AGENT_DEFINITION_STRING_BYTES: usize = 64 * 1024;

impl Default for AppPackageLimits {
    fn default() -> Self {
        Self {
            max_manifest_bytes: 131_072,
            max_yaml_depth: 32,
            max_yaml_nodes: 10_000,
            max_string_bytes: 4_096,
            max_entities: 128,
            max_fields: 2_048,
            max_views: 128,
            max_workflows: 128,
            max_actions: 256,
            max_assets: 256,
            max_dependencies: 256,
            max_bundle_files: 1_024,
            max_bundle_file_bytes: 16 * 1_024 * 1_024,
            max_bundle_bytes: 64 * 1_024 * 1_024,
            max_bundle_path_bytes: ABSOLUTE_MAX_BUNDLE_PATH_BYTES,
        }
    }
}

impl AppPackageLimits {
    #[cfg(any(test, feature = "test-fixtures"))]
    pub(crate) fn for_bounded_yaml(max_bytes: usize) -> Self {
        Self {
            max_manifest_bytes: max_bytes,
            ..Self::default()
        }
    }

    /// Limits for an **agent definition**, which is not an app manifest.
    ///
    /// `max_string_bytes` exists to stop an app manifest smuggling a payload
    /// through a declaration field; 4 KiB is generous for a name, a route or a
    /// description. An agent definition's `persona` is prose — the shipped
    /// `personal-assistant` persona is a little over 6 KiB — so the manifest
    /// limit is simply the wrong instrument here.
    ///
    /// This was not a theoretical mismatch. Applying the manifest limit made
    /// the reserved default agent definition invalid, which failed the whole
    /// agent catalog scan, which marked the primitive catalog unavailable,
    /// which made EVERY app capability unresolvable. Since a real deployment
    /// always has that agent seeded, the app platform could not admit, review
    /// or approve a single package.
    ///
    /// Every other bound — depth, node count, total bytes — is unchanged;
    /// prose being long is not the same as structure being unbounded.
    pub(crate) fn for_bounded_agent_definition(max_bytes: usize) -> Self {
        Self {
            max_manifest_bytes: max_bytes,
            max_string_bytes: MAX_AGENT_DEFINITION_STRING_BYTES,
            ..Self::default()
        }
    }

    pub const fn max_dependencies(&self) -> usize {
        self.max_dependencies
    }

    pub const fn max_string_bytes(&self) -> usize {
        self.max_string_bytes
    }

    pub const fn max_bundle_files(&self) -> usize {
        self.max_bundle_files
    }

    pub const fn max_bundle_file_bytes(&self) -> usize {
        self.max_bundle_file_bytes
    }

    pub const fn max_bundle_bytes(&self) -> usize {
        self.max_bundle_bytes
    }

    pub const fn max_bundle_path_bytes(&self) -> usize {
        self.max_bundle_path_bytes
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPackageManifest {
    pub name: AppName,
    pub version: String,
    pub description: String,
    pub metadata: AppManifestMetadata,
    pub app: AppManifestBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestMetadata {
    pub magician: AppManifestMagicianMetadata,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestSkillType {
    App,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestMagicianMetadata {
    pub skill_type: AppManifestSkillType,
    pub app_manifest_version: String,
    /// Deprecated generation marker retained for V1 package compatibility.
    /// It is informational and is not an admission or authority check.
    #[serde(default = "default_legacy_authoring_sdk_version")]
    pub app_sdk_version: String,
    /// Platform behavior the package requires. Admission checks these
    /// capabilities independently of whichever SDK happened to generate it.
    /// Empty means the legacy V1 manifest baseline only; it never opts into a
    /// future feature implicitly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_features: Vec<AppManifestFeature>,
    /// Optional informational generator identity. This never grants features.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_by: Option<AppManifestGeneratedBy>,
}

fn default_legacy_authoring_sdk_version() -> String {
    "unknown".to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestBody {
    pub compatibility: BTreeMap<AppName, String>,
    /// Review-visible package class. `System` is a claim that untrusted
    /// publication paths must refuse; only a separately digest-pinned,
    /// host-provenance boot path may admit it as system authority.
    #[serde(default, skip_serializing_if = "is_installable_distribution")]
    pub distribution: AppManifestDistribution,
    pub data_policy: AppManifestDataPolicy,
    pub entities: BTreeMap<AppName, AppManifestEntity>,
    pub views: BTreeMap<AppName, AppManifestView>,
    pub workflows: BTreeMap<AppName, AppManifestWorkflow>,
    pub actions: BTreeMap<AppName, AppManifestAction>,
    pub resources: AppManifestResources,
    #[serde(default)]
    pub dependencies: AppManifestDependencies,
    /// Named LLM operations the package wants to route like core lanes
    /// (plan 1.4, `llm_operations_v1`). Absent or empty means the package
    /// declares none and resolves exactly as before; names declared here are
    /// review material only — admission stays with the operator's
    /// `app_platform.llm_operations` policy and routing with
    /// `llm.router.operation_mapping` over trusted processing profiles.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub llm_operations: BTreeMap<AppName, AppManifestLlmOperation>,
    /// Permission vocabulary the package requests (plan 1.6,
    /// `custom_surfaces_v1`). Each entry names a capability class whose
    /// reviewable declaration block must also be present; an unknown or
    /// duplicate permission is refused at parse time. Absent means the
    /// package requests nothing beyond the pre-1.6 baseline, and every
    /// behavior that existed before the vocabulary is unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<AppManifestPermission>,
    /// Declaration block behind the `custom_surface` permission: the
    /// per-surface scripted entry points a package may host. `None` is the
    /// pre-1.6 baseline (no scripted hosting); `Some` requires the
    /// `custom_surface` permission and the `custom_surfaces_v1` feature,
    /// and must pass [`validate_custom_surface_declaration`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_surface: Option<AppManifestCustomSurfaceDeclaration>,
    /// Additive `app_widgets_v1` declarations. They are inert review material
    /// here; render/evaluator/slot consumers must reopen live installation
    /// authority and apply their own package/generation fences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub widgets: Vec<AppManifestWidget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indicators: Vec<AppManifestIndicator>,
    /// First-party route/nav declarations. Only system-class manifests may
    /// carry these, and `System` remains provenance-untrusted at this layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub navigation: Vec<AppManifestNavigationEntry>,
    /// Additive `app_behaviors_v1` declarations. A behavior binds one
    /// scheduled action to an explicit cadence and the complete set of named
    /// app LLM operations it may use. This is inert review material; it does
    /// not create a timer or execution authority.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub behaviors: Vec<AppManifestBehavior>,
    /// Additive `app_event_behaviors_v1` review declarations. Event behavior
    /// input is a host-owned canonical projection, never an own-store selector
    /// or arbitrary manifest filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_behaviors: Vec<AppManifestEventBehavior>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assets: Vec<AppBundlePath>,
    /// Additive `app_memory_read_v1` request for owner memory. Inert review
    /// material: the owner grants a subset at install (split interactive /
    /// background) and can change it later. Absent means the app requests no
    /// owner memory, and the field is skipped so existing manifests keep
    /// byte-identical digests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<super::memory_access::AppManifestMemory>,
}

fn is_installable_distribution(value: &AppManifestDistribution) -> bool {
    *value == AppManifestDistribution::Installable
}

/// At most eight declared custom-surface entry points per package. This
/// matches the bridge watchdog's eight concurrent sessions per
/// installation so no entry point can exist that cannot be individually
/// supervised (plan 1.6 design, section 2).
pub const APP_CUSTOM_SURFACE_MAX_ENTRY_POINTS: usize = 8;

/// Total executable custom-surface bytes (`.js`/`.mjs` members under
/// `surfaces/`) a single package may ship: 8 MiB by default. Package-wide
/// bundle caps stay unchanged; this is the narrower cap the review can
/// actually read (plan 1.6 design, section 2).
pub const APP_CUSTOM_SURFACE_MAX_EXECUTABLE_BYTES: usize = 8 * 1_024 * 1_024;

/// One declared scripted surface entry point: a `surfaces/`-relative HTML
/// document plus the bounded, canonical, installation-scoped route segment
/// it mounts at. Only declared entry documents may be loaded as top-level
/// surface documents; every other asset resolves relative to the same
/// verified revision through the existing resolver rules.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestCustomSurfaceEntryPoint {
    pub route: AppRoute,
    pub document: AppBundlePath,
}

/// The `app.custom_surface` declaration block behind the
/// `custom_surface` permission. There is deliberately no network field:
/// V1 surfaces have no egress channel, and a manifest attempting to
/// declare one is invalid (unknown field), not narrowed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestCustomSurfaceDeclaration {
    pub entry_points: Vec<AppManifestCustomSurfaceEntryPoint>,
}

impl AppManifestCustomSurfaceDeclaration {
    pub fn entry_points(&self) -> &[AppManifestCustomSurfaceEntryPoint] {
        &self.entry_points
    }
}

/// Manifest ceilings for the additive widget/indicator declaration surface.
/// Runtime response budgets may narrow these values but must never widen them.
pub const APP_WIDGET_MAX_DECLARATIONS: usize = 32;
pub const APP_INDICATOR_MAX_DECLARATIONS: usize = 16;
pub const APP_WIDGET_MAX_ACTIONS: usize = 8;
pub const APP_WIDGET_MAX_SUGGESTED_SLOTS: usize = 8;
pub const APP_WIDGET_MAX_READ_FIELDS: usize = 16;
pub const APP_WIDGET_MAX_ROWS: u16 = 100;
pub const APP_WIDGET_MAX_RENDER_BYTES: u32 = 64 * 1024;
pub const APP_SURFACING_MAX_TITLE_BYTES: usize = 256;
pub const APP_WIDGET_MAX_ACTION_LABEL_BYTES: usize = 64;
pub const APP_WIDGET_MIN_REFRESH_SECONDS: u32 = 5;
pub const APP_WIDGET_MAX_REFRESH_SECONDS: u32 = 24 * 60 * 60;
pub const APP_WIDGET_MAX_STALENESS_SECONDS: u32 = 7 * 24 * 60 * 60;
pub const APP_INDICATOR_MAX_TEXT_BYTES: u16 = 256;
pub const APP_INDICATOR_MAX_BADGE_VALUE: u32 = 9_999;
/// A selector is review material: a handful of equality terms a person reads
/// at a glance, not a query language.
pub const APP_INDICATOR_MAX_FILTER_TERMS: usize = 4;
/// A filter term compares against one short literal, not a payload.
pub const APP_INDICATOR_MAX_FILTER_VALUE_BYTES: usize = 256;
pub const APP_NAVIGATION_MAX_ENTRIES: usize = 16;
/// Longest single segment a first-party navigation route may carry.
/// Deliberately tighter than the 256-byte whole-route budget every other
/// route obeys, because this route is mirrored by each shell's directory
/// parser and the tightest of those caps is the one that decides whether a
/// declaration can be decoded at all.
pub const APP_NAVIGATION_MAX_ROUTE_SEGMENT_BYTES: usize = 128;
pub const APP_BEHAVIOR_MAX_DECLARATIONS: usize = 32;
pub const APP_BEHAVIOR_MAX_OPERATIONS: usize = 16;
/// A recipe may step more times than it has distinct operations (an operation
/// can legitimately run twice), but not without bound: every step is a model
/// call charged against the behavior's budget.
pub const APP_BEHAVIOR_MAX_STEPS: usize = 16;
/// A guard compares against one short literal, not a payload.
pub const APP_BEHAVIOR_MAX_GUARD_VALUE_BYTES: usize = 256;
pub const APP_BEHAVIOR_MAX_INPUT_FIELDS: usize = 32;
pub const APP_BEHAVIOR_MAX_PURPOSE_BYTES: usize = 256;
pub const APP_BEHAVIOR_MIN_INTERVAL_SECONDS: u64 = 60;
pub const APP_BEHAVIOR_MAX_INTERVAL_SECONDS: u64 = 31 * 24 * 60 * 60;
pub const APP_BEHAVIOR_MAX_PERIOD_SECONDS: u64 = 31 * 24 * 60 * 60;
pub const APP_BEHAVIOR_MAX_STARTS_PER_PERIOD: u32 = 10_000;
pub const APP_BEHAVIOR_MAX_CAUSATION_DEPTH: u16 = 16;
pub const APP_BEHAVIOR_MAX_SPEND_DEPTH: u16 = 16;
pub const APP_BEHAVIOR_MAX_CONTRIBUTION_PROPOSALS_PER_RUN: u16 = 64;
pub const APP_EVENT_BEHAVIOR_MAX_DECLARATIONS: usize = 32;
pub const APP_NOTIFICATION_MAX_PORTS_PER_WORKFLOW: usize = 16;
pub const APP_NOTIFICATION_MAX_TOTAL_PORTS: usize = 32;
pub const APP_NOTIFICATION_MAX_PURPOSE_BYTES: usize = 256;
pub const APP_NOTIFICATION_MIN_PERIOD_SECONDS: u64 = 60;
pub const APP_NOTIFICATION_MAX_PERIOD_SECONDS: u64 = 31 * 24 * 60 * 60;
pub const APP_NOTIFICATION_MAX_PER_PERIOD: u32 = 10_000;
pub const APP_NOTIFICATION_MAX_PENDING: u16 = 100;
pub const APP_NOTIFICATION_MIN_TTL_SECONDS: u64 = 60;
pub const APP_NOTIFICATION_MAX_TTL_SECONDS: u64 = 31 * 24 * 60 * 60;

/// Closed, non-executable V1 widget read. A declaration can project only
/// fields from one package-owned entity through an existing declarative view.
/// Binder-action reads remain deliberately absent until a scoped host owner
/// can execute them without inventing workflow-free tool authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestWidgetRead {
    ViewProjection {
        view: AppName,
        entity: AppName,
        fields: Vec<AppName>,
    },
}

impl AppManifestWidgetRead {
    pub fn view(&self) -> &AppName {
        match self {
            Self::ViewProjection { view, .. } => view,
        }
    }

    pub fn entity(&self) -> &AppName {
        match self {
            Self::ViewProjection { entity, .. } => entity,
        }
    }

    pub fn fields(&self) -> &[AppName] {
        match self {
            Self::ViewProjection { fields, .. } => fields,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestWidgetRefreshHint {
    pub min_interval_seconds: u32,
    pub max_staleness_seconds: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestWidgetBounds {
    pub max_rows: u16,
    pub max_render_bytes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestWidgetAction {
    pub id: AppName,
    pub label: String,
    pub governed_action: AppName,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestWidgetClientCapability {
    DeclarativeListV1,
    DeclarativeTableV1,
    DeclarativeTreeV1,
    DeclarativeTimelineV1,
    DeclarativeGraphV1,
    GovernedActionsV1,
    MiniFrameV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestWidgetRendering {
    Native,
    MiniFrame { entry_point: AppRoute },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestWidgetFallback {
    Unavailable,
    View { view: AppName },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestSuggestedSlot {
    pub page: AppRoute,
    pub slot: AppName,
    #[serde(default)]
    pub system_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestWidget {
    pub id: AppName,
    pub title: String,
    pub view: AppName,
    pub read: AppManifestWidgetRead,
    pub refresh_hint: AppManifestWidgetRefreshHint,
    pub bounds: AppManifestWidgetBounds,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<AppManifestWidgetAction>,
    pub rendering: AppManifestWidgetRendering,
    pub fallback: AppManifestWidgetFallback,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_slots: Vec<AppManifestSuggestedSlot>,
    pub required_capabilities: Vec<AppManifestWidgetClientCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestIndicatorProjection {
    Chip { field: AppName },
    Badge { field: AppName },
    State { field: AppName },
}

impl AppManifestIndicatorProjection {
    pub fn field(&self) -> &AppName {
        match self {
            Self::Chip { field } | Self::Badge { field } | Self::State { field } => field,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestIndicatorBounds {
    pub max_text_bytes: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_badge_value: Option<u32>,
}

/// One equality term of an indicator selector.
///
/// The literal lives in the reviewed manifest, never in a request: a caller
/// may name an indicator but can never influence which record it reads.
/// `equals` is text for the same reason a behavior step guard's is — one
/// short literal a reviewer reads at a glance — and [`AppManifestIndicatorFilter::literal`]
/// is the single place it is given a type. A non-text field's literal is
/// therefore quoted in YAML, and a bare scalar is a parse error rather than a
/// silently retyped value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestIndicatorFilter {
    pub field: AppName,
    pub equals: String,
}

/// The typed literal a filter term compares against.
///
/// Validation and the runtime compiler both read a declared literal through
/// [`AppManifestIndicatorFilter::literal`], so a manifest can never validate
/// under one reading of `equals` and compile under another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppManifestIndicatorLiteral {
    Text(String),
    Enum(AppName),
    Integer(i64),
    Boolean(bool),
}

impl AppManifestIndicatorFilter {
    /// The typed literal this term compares against, or `None` when the
    /// declared field kind cannot carry an exact equality.
    ///
    /// Markdown, decimal, timestamp and reference fields are absent on
    /// purpose: a decimal or timestamp equality depends on how a value was
    /// spelled, a markdown body is not a selector, and a reference literal
    /// would be a record id frozen into review material.
    pub fn literal(&self, field: &AppManifestField) -> Option<AppManifestIndicatorLiteral> {
        match field {
            AppManifestField::Text { .. } => {
                Some(AppManifestIndicatorLiteral::Text(self.equals.clone()))
            },
            AppManifestField::Enum { values, .. } => values
                .iter()
                .find(|value| value.as_str() == self.equals)
                .cloned()
                .map(AppManifestIndicatorLiteral::Enum),
            AppManifestField::Integer { .. } => self
                .equals
                .parse::<i64>()
                .ok()
                .map(AppManifestIndicatorLiteral::Integer),
            AppManifestField::Boolean { .. } => match self.equals.as_str() {
                "true" => Some(AppManifestIndicatorLiteral::Boolean(true)),
                "false" => Some(AppManifestIndicatorLiteral::Boolean(false)),
                _ => None,
            },
            AppManifestField::Markdown { .. }
            | AppManifestField::Decimal { .. }
            | AppManifestField::Timestamp { .. }
            | AppManifestField::Reference { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestIndicatorOrderDirection {
    Ascending,
    Descending,
}

/// The scalar kinds an `ordered_first` selector may order by.
///
/// These are the two kinds whose comparison the indicator evaluator can
/// re-check exactly against the two rows the store returned. Ordering by
/// anything else would leave the evaluator guessing at the store's own
/// comparison, and that guess is what this contract exists to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppManifestIndicatorOrderKind {
    Integer,
    Timestamp,
}

impl AppManifestIndicatorOrderKind {
    /// `None` for every field an ordered-first selector may not order by,
    /// including an optional or nullable one: a record whose order value can
    /// be absent cannot be ranked against its runner-up, so it could never be
    /// shown to be the leader.
    pub fn of(field: &AppManifestField) -> Option<Self> {
        if !field.required() || field.nullable() {
            return None;
        }
        match field {
            AppManifestField::Integer { .. } => Some(Self::Integer),
            AppManifestField::Timestamp { .. } => Some(Self::Timestamp),
            AppManifestField::Text { .. }
            | AppManifestField::Markdown { .. }
            | AppManifestField::Decimal { .. }
            | AppManifestField::Boolean { .. }
            | AppManifestField::Enum { .. }
            | AppManifestField::Reference { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestIndicatorOrder {
    pub field: AppName,
    pub direction: AppManifestIndicatorOrderDirection,
}

/// The exact-selector contract a compiled indicator must carry.
///
/// The first surfacing slice shipped indicators with no selector at all, so
/// the only thing a host could compile was an unfiltered one-row read: which
/// record a chip described was then decided by store order. Each variant here
/// names the record instead, and each one is refused at evaluation time when
/// the read does not prove the choice, so a chip is hidden rather than
/// guessed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestIndicatorSelector {
    /// The read's entity holds exactly one record. Declaring this is what
    /// separates a deliberate singleton from an unfiltered `LIMIT 1`; a
    /// second record hides the indicator rather than picking between them.
    SoleRecord,
    /// The one record matching a closed conjunction of equality terms. A
    /// filter that matches a second record hides the indicator.
    ExactRecord {
        filter: Vec<AppManifestIndicatorFilter>,
    },
    /// The leader under an explicit total order, optionally narrowed by the
    /// same equality terms. The evaluator reads the runner-up too and hides
    /// the indicator unless the leader strictly outranks it, so a tie can
    /// never be broken by store order.
    OrderedFirst {
        order: AppManifestIndicatorOrder,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        filter: Vec<AppManifestIndicatorFilter>,
    },
}

impl AppManifestIndicatorSelector {
    pub fn filter(&self) -> &[AppManifestIndicatorFilter] {
        match self {
            Self::SoleRecord => &[],
            Self::ExactRecord { filter } | Self::OrderedFirst { filter, .. } => filter,
        }
    }

    pub fn order(&self) -> Option<&AppManifestIndicatorOrder> {
        match self {
            Self::SoleRecord | Self::ExactRecord { .. } => None,
            Self::OrderedFirst { order, .. } => Some(order),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestIndicator {
    pub id: AppName,
    pub title: String,
    pub read: AppManifestWidgetRead,
    pub refresh_hint: AppManifestWidgetRefreshHint,
    pub projection: AppManifestIndicatorProjection,
    pub bounds: AppManifestIndicatorBounds,
    /// Which record this indicator is about. Absent is the pre-contract
    /// shape: still valid review material, still parsed unchanged, and still
    /// inert, because nothing else in the declaration answers the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<AppManifestIndicatorSelector>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestNavigationPlacement {
    Route,
    Tab { tab: AppName },
    Section { section: AppName },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestNavigationSurface {
    View {
        view: AppName,
    },
    CustomSurface {
        entry_point: AppRoute,
        fallback_view: AppName,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestNavigationEntry {
    pub id: AppName,
    pub title: String,
    pub route: AppRoute,
    pub placement: AppManifestNavigationPlacement,
    pub surface: AppManifestNavigationSurface,
}

/// Closed V1 cadence vocabulary. Calendar/cron/timezone expressions are
/// deliberately absent: the first increment has one deterministic UTC
/// interval policy, so DST and catch-up cannot be smuggled through a string.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestBehaviorCadence {
    Interval { min_interval_seconds: u64 },
}

impl AppManifestBehaviorCadence {
    pub fn min_interval_seconds(self) -> u64 {
        match self {
            Self::Interval {
                min_interval_seconds,
            } => min_interval_seconds,
        }
    }
}

/// One scheduled behavior request. `operations` is an unordered allow-set,
/// never an implicit execution sequence; it may be empty for a canonical
/// deterministic recipe. `output_schema` is the only structured model-output
/// shape the dispatcher may request for this behavior, so it is invalid when
/// `operations` is empty.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestBehavior {
    pub id: AppName,
    pub cadence: AppManifestBehaviorCadence,
    pub purpose: String,
    pub action: AppName,
    /// Exact package-owned singleton projected into the scheduled workflow
    /// input. No predicate, sort, cursor, ambient scope, or first-row
    /// interpretation exists in V1.
    pub input: AppManifestBehaviorInputSelector,
    /// The unordered ALLOW-SET of operations this behavior may use. It says
    /// which operations are permitted, never in what order, and is never read
    /// as a recipe — not by position and not by prose.
    pub operations: Vec<AppName>,
    /// The ordered, reviewed recipe. This is the immutable operation-step
    /// aggregate the behaviors increment named as the precondition for a
    /// behavior reaching the LLM dispatcher at all.
    ///
    /// Required whenever `operations` is non-empty: an allow-set with no
    /// recipe cannot be executed without guessing, and guessing is exactly
    /// what the increment refused to do.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<AppManifestBehaviorStep>,
    /// Legacy single-schema form, kept for behaviors that declare no `steps`.
    /// Mutually exclusive with `steps`, whose members own their own schemas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<AppManifestInputSchema>,
}

/// One step of a behavior's reviewed operation recipe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestBehaviorStep {
    pub id: AppName,
    /// Must name one of the behavior's declared `operations`.
    pub operation: AppName,
    /// Structured output this step must produce. Per-step rather than
    /// per-behavior so a guard has an exact, validated shape to read.
    pub output_schema: AppManifestInputSchema,
    /// Run this step only when an earlier step's validated output matches.
    /// Absent means always run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<AppManifestBehaviorStepGuard>,
}

/// A closed equality guard against one earlier step's validated output.
///
/// Deliberately not an expression language. Equality against one named field
/// of one named earlier step is enough to express "compose only if the gate
/// said engage", and it is something a reviewer can read in full.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestBehaviorStepGuard {
    /// Id of an EARLIER step. Self and forward references are refused, so a
    /// recipe cannot describe a cycle.
    pub step: AppName,
    pub field: AppName,
    pub equals: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestBehaviorInputSelector {
    pub entity: AppName,
    pub record_id: AppRecordId,
    pub fields: Vec<AppName>,
}

/// One event-driven behavior request. The subscription grammar is a closed,
/// same-installation projection selector; arbitrary predicates, workspace
/// scope, transport event names and caller-chosen dedupe keys are absent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestEventBehavior {
    pub id: AppName,
    pub purpose: String,
    pub action: AppName,
    pub subscription: AppEventSubscriptionV1,
    pub min_interval_seconds: u64,
    /// Unordered allow-set, exactly as on a scheduled behavior.
    pub operations: Vec<AppName>,
    /// The ordered, reviewed recipe. Same rules and same reason as
    /// [`AppManifestBehavior::steps`]: an event-driven behavior's operations
    /// are no more self-ordering than a scheduled one's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<AppManifestBehaviorStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<AppManifestInputSchema>,
}

/// One workflow-local, one-way notification port. Runtime payload shape,
/// correlation, idempotency and destination scope remain host-owned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestNotificationPort {
    pub kind: AppNotificationKindV1,
    pub purpose: String,
    pub severity_ceiling: AppNotificationSeverityV1,
    pub max_notifications_per_period: u32,
    pub period_seconds: u64,
    pub max_pending: u16,
    pub ttl_seconds: u64,
}

pub fn event_behavior_projection_schema(
    subscription: &AppEventSubscriptionV1,
) -> AppManifestInputSchema {
    let AppEventSubscriptionV1::InstallationExecutionTerminal { outcomes } = subscription;
    let outcome_values = outcomes
        .iter()
        .map(|outcome| {
            AppName::parse(match outcome {
                AppEventTerminalOutcomeV1::Succeeded => "succeeded",
                AppEventTerminalOutcomeV1::Failed => "failed",
                AppEventTerminalOutcomeV1::Cancelled => "cancelled",
            })
            .expect("static app event outcome names are valid")
        })
        .collect::<BTreeSet<_>>();
    let event_ref = AppName::parse("event_ref").expect("static app event field is valid");
    let execution_ref = AppName::parse("execution_ref").expect("static app event field is valid");
    let outcome = AppName::parse("outcome").expect("static app event field is valid");
    let recorded_at = AppName::parse("recorded_at").expect("static app event field is valid");
    AppManifestInputSchema {
        schema_type: AppManifestSchemaType::Object,
        fields: BTreeMap::new(),
        value_schema: Some(AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root: 4,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Sensitive,
                model_processing: AppModelProcessing::LocalOnly,
            },
            nodes: vec![
                AppWorkflowValueTypeNode::Text { max_bytes: 256 },
                AppWorkflowValueTypeNode::Text { max_bytes: 256 },
                AppWorkflowValueTypeNode::Enum {
                    values: outcome_values,
                },
                AppWorkflowValueTypeNode::Timestamp,
                AppWorkflowValueTypeNode::Record {
                    fields: BTreeMap::from([
                        (
                            event_ref,
                            AppWorkflowRecordField {
                                value_type: 0,
                                required: true,
                            },
                        ),
                        (
                            execution_ref,
                            AppWorkflowRecordField {
                                value_type: 1,
                                required: true,
                            },
                        ),
                        (
                            outcome,
                            AppWorkflowRecordField {
                                value_type: 2,
                                required: true,
                            },
                        ),
                        (
                            recorded_at,
                            AppWorkflowRecordField {
                                value_type: 3,
                                required: true,
                            },
                        ),
                    ]),
                },
            ],
        }),
    }
}

/// Validate the additive `app.permissions` + `app.custom_surface`
/// declaration (plan 1.6, `custom_surfaces_v1`).
///
/// Manifest-validation-time discipline, mirroring `llm_operations`: the
/// block is inert review material here, rejected unless the package
/// explicitly requires `custom_surfaces_v1` and names the
/// `custom_surface` permission. Entry-point documents are checked for
/// existence, shape, and the executable-byte cap against the bundle in
/// [`validate_custom_surface_bundle_members`] where members are known.
fn validate_custom_surface_declaration(
    manifest: &AppPackageManifest,
) -> Result<(), AppManifestError> {
    let permissions = &manifest.app.permissions;
    if permissions.is_empty() {
        if manifest.app.custom_surface.is_some() {
            return Err(AppManifestError::InvalidManifest(
                "app declares custom_surface without the custom_surface permission".to_owned(),
            ));
        }
        return Ok(());
    }
    let mut seen = BTreeSet::new();
    for permission in permissions {
        if !AppManifestPermission::supported().contains(permission) {
            return Err(AppManifestError::InvalidManifest(format!(
                "app.permissions contains unsupported permission {permission:?}"
            )));
        }
        if !seen.insert(*permission) {
            return Err(AppManifestError::InvalidManifest(
                "app.permissions contains duplicates".to_owned(),
            ));
        }
    }
    let Some(declaration) = manifest.app.custom_surface.as_ref() else {
        return Err(AppManifestError::InvalidManifest(
            "app declares the custom_surface permission without a custom_surface block".to_owned(),
        ));
    };
    if !manifest
        .metadata
        .magician
        .required_features
        .contains(&AppManifestFeature::CustomSurfacesV1)
    {
        return Err(AppManifestError::InvalidManifest(
            "app declares custom_surface without required feature custom_surfaces_v1".to_owned(),
        ));
    }
    let entry_points = &declaration.entry_points;
    if entry_points.is_empty() {
        return Err(AppManifestError::InvalidManifest(
            "app.custom_surface.entry_points must declare at least one entry point".to_owned(),
        ));
    }
    validate_vec_limit(
        "custom_surface.entry_points",
        entry_points.len(),
        APP_CUSTOM_SURFACE_MAX_ENTRY_POINTS,
    )?;
    let mut routes = BTreeSet::new();
    let mut documents = BTreeSet::new();
    for entry in entry_points {
        if !routes.insert(entry.route.collision_key()) {
            return Err(AppManifestError::DuplicateNormalizedValue {
                field: "custom_surface.entry_points.route",
            });
        }
        if !documents.insert(entry.document.collision_key()) {
            return Err(AppManifestError::DuplicateNormalizedValue {
                field: "custom_surface.entry_points.document",
            });
        }
        ensure_custom_surface_document_path(&entry.document)?;
        for view in manifest.app.views.values() {
            if app_route_templates_overlap(entry.route.as_str(), view.route.as_str()) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "custom-surface route `{}` overlaps declared view route `{}`",
                    entry.route.as_str(),
                    view.route.as_str()
                )));
            }
        }
    }
    Ok(())
}

/// Bundle-member half of the custom-surface declaration: entry documents
/// must be regular members of this exact bundle, `.wasm` members under
/// `surfaces/` are refused while the capability is declared (WASM stays
/// refused until a wasm-specific threat review exists), and total
/// executable `.js`/`.mjs` bytes under `surfaces/` stay within the
/// narrower review-readable cap.
fn validate_custom_surface_bundle_members(
    manifest: &AppPackageManifest,
    members: &[AppValidatedBundleMember],
) -> Result<(), AppManifestError> {
    let Some(declaration) = manifest.app.custom_surface.as_ref() else {
        return Ok(());
    };
    let mut executable_bytes = 0usize;
    for member in members {
        let path = member.path.as_str();
        if !path.starts_with("surfaces/") {
            continue;
        }
        if member_has_custom_surface_executable_extension(path) {
            executable_bytes = executable_bytes.saturating_add(member.bytes.len());
        }
        if path.to_ascii_lowercase().ends_with(".wasm") {
            return Err(AppManifestError::InvalidManifest(format!(
                "custom-surface wasm is refused until a wasm threat review exists: {path}"
            )));
        }
    }
    if executable_bytes > APP_CUSTOM_SURFACE_MAX_EXECUTABLE_BYTES {
        return Err(AppManifestError::InvalidManifest(format!(
            "custom-surface executable members total {} bytes; the cap is {}",
            executable_bytes, APP_CUSTOM_SURFACE_MAX_EXECUTABLE_BYTES
        )));
    }
    let existing = members
        .iter()
        .map(|member| &member.path)
        .collect::<BTreeSet<_>>();
    for entry in &declaration.entry_points {
        if !existing.contains(&entry.document) {
            return Err(AppManifestError::MissingBundleMember(
                entry.document.to_string(),
            ));
        }
    }
    Ok(())
}

/// Case-insensitive executable-member predicate shared by the manifest
/// kernel's 8 MiB cap and the owner-review executable inventory, so a
/// swapped-case `.JS`/`.MJS` member can never count toward the cap while
/// escaping the review inventory, scan, and digest.
pub fn member_has_custom_surface_executable_extension(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    lowered.ends_with(".js") || lowered.ends_with(".mjs")
}

fn ensure_custom_surface_document_path(path: &AppBundlePath) -> Result<(), AppManifestError> {
    let raw = path.as_str();
    if !raw.starts_with("surfaces/")
        || raw == "surfaces/"
        || !raw.to_ascii_lowercase().ends_with(".html")
    {
        return Err(AppManifestError::InvalidManifest(format!(
            "custom-surface entry document must be a surfaces/ HTML member: {raw}"
        )));
    }
    Ok(())
}

fn validate_widget_declarations(
    manifest: &AppPackageManifest,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    let has_feature = manifest
        .metadata
        .magician
        .required_features
        .contains(&AppManifestFeature::AppWidgetsV1);
    let has_declarations = !manifest.app.widgets.is_empty()
        || !manifest.app.indicators.is_empty()
        || !manifest.app.navigation.is_empty();
    if has_declarations && !has_feature {
        return Err(AppManifestError::InvalidManifest(
            "app declares widgets, indicators, or navigation without required feature app_widgets_v1"
                .to_owned(),
        ));
    }
    if has_feature && !has_declarations {
        return Err(AppManifestError::InvalidManifest(
            "app requires app_widgets_v1 without any widget, indicator, or navigation declaration"
                .to_owned(),
        ));
    }
    if !manifest.app.navigation.is_empty()
        && manifest.app.distribution != AppManifestDistribution::System
    {
        return Err(AppManifestError::InvalidManifest(
            "first-party navigation may be declared only by a system-distribution manifest"
                .to_owned(),
        ));
    }
    validate_vec_limit(
        "widgets",
        manifest.app.widgets.len(),
        APP_WIDGET_MAX_DECLARATIONS,
    )?;
    validate_vec_limit(
        "indicators",
        manifest.app.indicators.len(),
        APP_INDICATOR_MAX_DECLARATIONS,
    )?;
    validate_vec_limit(
        "navigation",
        manifest.app.navigation.len(),
        APP_NAVIGATION_MAX_ENTRIES,
    )?;

    let mut widget_ids = BTreeSet::new();
    for widget in &manifest.app.widgets {
        validate_widget_title("widget.title", &widget.title, limits)?;
        if !widget_ids.insert(widget.id.clone()) {
            return Err(AppManifestError::InvalidManifest(format!(
                "widget id `{}` is declared more than once",
                widget.id
            )));
        }
        validate_widget_read(manifest, &widget.read)?;
        if widget.read.view() != &widget.view {
            return Err(AppManifestError::InvalidManifest(format!(
                "widget `{}` view does not match its closed read projection",
                widget.id
            )));
        }
        validate_widget_refresh(widget.refresh_hint, &widget.id)?;
        if widget.bounds.max_rows == 0
            || widget.bounds.max_rows > APP_WIDGET_MAX_ROWS
            || widget.bounds.max_render_bytes == 0
            || widget.bounds.max_render_bytes > APP_WIDGET_MAX_RENDER_BYTES
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "widget `{}` bounds exceed the app_widgets_v1 render ceilings",
                widget.id
            )));
        }
        validate_vec_limit(
            "widget.actions",
            widget.actions.len(),
            APP_WIDGET_MAX_ACTIONS,
        )?;
        let mut action_ids = BTreeSet::new();
        let mut governed_action_ids = BTreeSet::new();
        for action in &widget.actions {
            validate_widget_title("widget.actions.label", &action.label, limits)?;
            if !action_ids.insert(action.id.clone()) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "widget `{}` repeats action id `{}`",
                    widget.id, action.id
                )));
            }
            if !governed_action_ids.insert(action.governed_action.clone()) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "widget `{}` repeats governed action `{}`",
                    widget.id, action.governed_action
                )));
            }
            let governed_action = manifest
                .app
                .actions
                .get(&action.governed_action)
                .ok_or_else(|| {
                    AppManifestError::InvalidManifest(format!(
                        "widget `{}` names undeclared governed action `{}`",
                        widget.id, action.governed_action
                    ))
                })?;
            // V1 has no row-to-action input mapping. A button may therefore
            // expose only an action whose declared workflow accepts the exact
            // empty object; permitting required inputs here would make the
            // eventual client invent an undeclared binding or smuggle input
            // authority through UI state.
            if let Some(workflow) = manifest.app.workflows.get(&governed_action.workflow) {
                if workflow
                    .input
                    .validate_value(&serde_json::json!({}))
                    .is_err()
                {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "widget `{}` governed action `{}` requires input but app_widgets_v1 declares no input binding",
                        widget.id, action.governed_action
                    )));
                }
            }
        }
        if !widget.actions.is_empty()
            && (!manifest
                .metadata
                .magician
                .required_features
                .contains(&AppManifestFeature::GovernedActionsV1)
                || !widget
                    .required_capabilities
                    .contains(&AppManifestWidgetClientCapability::GovernedActionsV1))
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "widget `{}` actions require governed_actions_v1 in both manifest and client capabilities",
                widget.id
            )));
        }

        let view = manifest.app.views.get(&widget.view).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "widget `{}` names missing view `{}`",
                widget.id, widget.view
            ))
        })?;
        let required_view_capability = widget_capability_for_view(view.kind).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "widget `{}` uses a view kind unsupported by app_widgets_v1",
                widget.id
            ))
        })?;
        let mut capabilities = BTreeSet::new();
        for capability in &widget.required_capabilities {
            if !capabilities.insert(*capability) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "widget `{}` repeats a required client capability",
                    widget.id
                )));
            }
        }
        if !capabilities.contains(&required_view_capability) {
            return Err(AppManifestError::InvalidManifest(format!(
                "widget `{}` omits the client capability required by its view",
                widget.id
            )));
        }
        match &widget.rendering {
            AppManifestWidgetRendering::Native => {
                if capabilities.contains(&AppManifestWidgetClientCapability::MiniFrameV1) {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "native widget `{}` cannot require mini_frame_v1",
                        widget.id
                    )));
                }
            },
            AppManifestWidgetRendering::MiniFrame { entry_point } => {
                if !capabilities.contains(&AppManifestWidgetClientCapability::MiniFrameV1)
                    || !custom_surface_entry_exists(manifest, entry_point)
                {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "mini-frame widget `{}` must require mini_frame_v1 and name a declared custom-surface entry point",
                        widget.id
                    )));
                }
                if !matches!(&widget.fallback, AppManifestWidgetFallback::View { .. }) {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "mini-frame widget `{}` requires a declarative native fallback view",
                        widget.id
                    )));
                }
            },
        }
        if let AppManifestWidgetFallback::View { view } = &widget.fallback {
            let fallback = manifest.app.views.get(view).ok_or_else(|| {
                AppManifestError::InvalidManifest(format!(
                    "widget `{}` fallback names missing view `{view}`",
                    widget.id
                ))
            })?;
            let fallback_capability =
                widget_capability_for_view(fallback.kind).ok_or_else(|| {
                    AppManifestError::InvalidManifest(format!(
                        "widget `{}` fallback uses a view kind unsupported by app_widgets_v1",
                        widget.id
                    ))
                })?;
            if !capabilities.contains(&fallback_capability) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "widget `{}` omits the client capability required by its fallback view",
                    widget.id
                )));
            }
        }
        validate_vec_limit(
            "widget.suggested_slots",
            widget.suggested_slots.len(),
            APP_WIDGET_MAX_SUGGESTED_SLOTS,
        )?;
        let mut slots = BTreeSet::new();
        for slot in &widget.suggested_slots {
            if slot.page.parameter_names().next().is_some() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "widget `{}` suggested slot page must be a static first-party route",
                    widget.id
                )));
            }
            if !slots.insert((slot.page.clone(), slot.slot.clone())) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "widget `{}` repeats a suggested page/slot pair",
                    widget.id
                )));
            }
            if slot.system_default && manifest.app.distribution != AppManifestDistribution::System {
                return Err(AppManifestError::InvalidManifest(format!(
                    "installable widget `{}` cannot claim a system slot default",
                    widget.id
                )));
            }
        }
    }

    let mut indicator_ids = BTreeSet::new();
    for indicator in &manifest.app.indicators {
        validate_widget_title("indicator.title", &indicator.title, limits)?;
        if !indicator_ids.insert(indicator.id.clone()) {
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator id `{}` is declared more than once",
                indicator.id
            )));
        }
        validate_widget_read(manifest, &indicator.read)?;
        validate_widget_refresh(indicator.refresh_hint, &indicator.id)?;
        if !indicator
            .read
            .fields()
            .contains(indicator.projection.field())
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator `{}` projects a field absent from its closed read",
                indicator.id
            )));
        }
        let entity = manifest
            .app
            .entities
            .get(indicator.read.entity())
            .expect("validated widget read entity");
        let field = entity
            .fields
            .get(indicator.projection.field())
            .expect("validated widget read field");
        if matches!(
            &indicator.projection,
            AppManifestIndicatorProjection::Badge { .. }
        ) && !matches!(field, AppManifestField::Integer { .. })
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator `{}` badge projection requires an integer field",
                indicator.id
            )));
        }
        let badge_bound_valid = match (&indicator.projection, indicator.bounds.max_badge_value) {
            (AppManifestIndicatorProjection::Badge { .. }, Some(value)) => {
                value > 0 && value <= APP_INDICATOR_MAX_BADGE_VALUE
            },
            (AppManifestIndicatorProjection::Badge { .. }, None) => false,
            (_, None) => true,
            (_, Some(_)) => false,
        };
        if indicator.bounds.max_text_bytes == 0
            || indicator.bounds.max_text_bytes > APP_INDICATOR_MAX_TEXT_BYTES
            || !badge_bound_valid
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator `{}` bounds exceed app_widgets_v1 ceilings",
                indicator.id
            )));
        }
        if let Some(selector) = indicator.selector.as_ref() {
            validate_indicator_selector(indicator, selector, entity, limits)?;
        }
    }

    let mut navigation_ids = BTreeSet::new();
    let mut navigation_routes = BTreeSet::new();
    for entry in &manifest.app.navigation {
        validate_widget_title("navigation.title", &entry.title, limits)?;
        if !navigation_ids.insert(entry.id.clone())
            || !navigation_routes.insert(entry.route.clone())
            || entry.route.parameter_names().next().is_some()
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "navigation entry `{}` has a duplicate or dynamic first-party route",
                entry.id
            )));
        }
        if !navigation_route_is_mountable(&entry.route) {
            return Err(AppManifestError::InvalidManifest(format!(
                "navigation entry `{}` must name a bounded destination below the site root",
                entry.id
            )));
        }
        match &entry.surface {
            AppManifestNavigationSurface::View { view } => {
                if !manifest.app.views.contains_key(view) {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "navigation entry `{}` names missing view `{view}`",
                        entry.id
                    )));
                }
            },
            AppManifestNavigationSurface::CustomSurface {
                entry_point,
                fallback_view,
            } => {
                if !custom_surface_entry_exists(manifest, entry_point)
                    || !manifest.app.views.contains_key(fallback_view)
                {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "navigation entry `{}` requires a declared custom entry point and declarative fallback view",
                        entry.id
                    )));
                }
                // The entry point travels to the shell inside the declaration
                // and is re-parsed under the same narrowed grammar as the
                // route above, so it has to clear the same bar. Custom-surface
                // routes at large are not narrowed here: only one named by a
                // navigation declaration is ever decoded that way.
                if !navigation_route_is_mountable(entry_point) {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "navigation entry `{}` names a custom entry point no shell can mount",
                        entry.id
                    )));
                }
            },
        }
    }
    Ok(())
}

/// The narrowing every shell applies on top of the host route grammar before
/// it will mount a declared destination: the site root is not a package's to
/// claim, and one segment stays inside the cap each client mirrors, which is
/// far tighter than the whole-route byte budget.
///
/// This is an admission rule rather than client-side taste because a directory
/// client that cannot decode one declared entry refuses the entire page. A
/// single manifest naming `/` would therefore not degrade its own app — it
/// would empty the Apps list for every installation in the scope.
fn navigation_route_is_mountable(route: &AppRoute) -> bool {
    route.as_str() != "/"
        && route
            .as_str()
            .split('/')
            .all(|segment| segment.len() <= APP_NAVIGATION_MAX_ROUTE_SEGMENT_BYTES)
}

/// Validate one indicator's exact-selector contract.
///
/// These rules exist so that "which record is this chip about?" has exactly
/// one answer a reviewer can read off the manifest. An indicator that
/// declares no selector is not validated here at all: it stays the inert
/// pre-contract shape, and the runtime compiler refuses to build a plan for
/// it rather than inventing a record.
fn validate_indicator_selector(
    indicator: &AppManifestIndicator,
    selector: &AppManifestIndicatorSelector,
    entity: &AppManifestEntity,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    let filter = selector.filter();
    validate_vec_limit(
        "indicator.selector.filter",
        filter.len(),
        APP_INDICATOR_MAX_FILTER_TERMS,
    )?;
    if matches!(selector, AppManifestIndicatorSelector::ExactRecord { .. }) && filter.is_empty() {
        // An exact record with nothing to match on is an unfiltered one-row
        // read wearing the word "exact". A package that means "this entity
        // holds one record" says so with `sole_record`.
        return Err(AppManifestError::InvalidManifest(format!(
            "indicator `{}` exact-record selector must match on at least one field; \
             declare `sole_record` for a singleton entity",
            indicator.id
        )));
    }
    let mut filtered = BTreeSet::new();
    for term in filter {
        // The closed read is what a reviewer approved. A selector that
        // narrowed on a field outside it would read records by a field the
        // declaration never showed.
        if !indicator.read.fields().contains(&term.field) {
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator `{}` selector matches on field `{}`, which is absent from its closed \
                 read",
                indicator.id, term.field
            )));
        }
        if !filtered.insert(term.field.clone()) {
            // Two equalities on one field are either redundant or
            // contradictory, and both read to a reviewer as a mistake.
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator `{}` selector matches field `{}` more than once",
                indicator.id, term.field
            )));
        }
        if term.equals.trim().is_empty()
            || term.equals.len()
                > limits
                    .max_string_bytes
                    .min(APP_INDICATOR_MAX_FILTER_VALUE_BYTES)
            || term.equals.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator `{}` selector value for field `{}` must be 1..={} non-control bytes",
                indicator.id, term.field, APP_INDICATOR_MAX_FILTER_VALUE_BYTES
            )));
        }
        let field = entity.fields.get(&term.field).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "indicator `{}` selector names missing field `{}`",
                indicator.id, term.field
            ))
        })?;
        // Without this an enum typo produces an indicator that silently never
        // matches, which looks identical to a working one until someone
        // notices the chip is never there.
        if term.literal(field).is_none() {
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator `{}` selector value `{}` is not an exact value of field `{}`",
                indicator.id, term.equals, term.field
            )));
        }
    }
    if let Some(order) = selector.order() {
        // The evaluator proves the leader beat the runner-up by comparing the
        // order value on the two projected rows, so the order field has to be
        // in the closed read as well as in the order.
        if !indicator.read.fields().contains(&order.field) {
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator `{}` orders by field `{}`, which is absent from its closed read",
                indicator.id, order.field
            )));
        }
        let field = entity.fields.get(&order.field).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "indicator `{}` orders by missing field `{}`",
                indicator.id, order.field
            ))
        })?;
        if AppManifestIndicatorOrderKind::of(field).is_none() {
            return Err(AppManifestError::InvalidManifest(format!(
                "indicator `{}` order field `{}` must be a required, non-nullable integer or \
                 timestamp",
                indicator.id, order.field
            )));
        }
    }
    Ok(())
}

fn validate_widget_read(
    manifest: &AppPackageManifest,
    read: &AppManifestWidgetRead,
) -> Result<(), AppManifestError> {
    if read.fields().is_empty() || read.fields().len() > APP_WIDGET_MAX_READ_FIELDS {
        return Err(AppManifestError::InvalidManifest(
            "widget/indicator view projection must select a bounded non-empty field list"
                .to_owned(),
        ));
    }
    let view = manifest.app.views.get(read.view()).ok_or_else(|| {
        AppManifestError::InvalidManifest(format!(
            "widget/indicator read names missing view `{}`",
            read.view()
        ))
    })?;
    if &view.entity != read.entity() {
        return Err(AppManifestError::InvalidManifest(format!(
            "widget/indicator read entity `{}` does not match view `{}`",
            read.entity(),
            read.view()
        )));
    }
    let entity = manifest.app.entities.get(read.entity()).ok_or_else(|| {
        AppManifestError::InvalidManifest(format!(
            "widget/indicator read names missing entity `{}`",
            read.entity()
        ))
    })?;
    let mut fields = BTreeSet::new();
    for field in read.fields() {
        if !fields.insert(field.clone()) || !entity.fields.contains_key(field) {
            return Err(AppManifestError::InvalidManifest(format!(
                "widget/indicator read repeats or names missing field `{field}`"
            )));
        }
    }
    Ok(())
}

fn validate_widget_refresh(
    refresh: AppManifestWidgetRefreshHint,
    id: &AppName,
) -> Result<(), AppManifestError> {
    if refresh.min_interval_seconds < APP_WIDGET_MIN_REFRESH_SECONDS
        || refresh.min_interval_seconds > APP_WIDGET_MAX_REFRESH_SECONDS
        || refresh.max_staleness_seconds < refresh.min_interval_seconds
        || refresh.max_staleness_seconds > APP_WIDGET_MAX_STALENESS_SECONDS
    {
        return Err(AppManifestError::InvalidManifest(format!(
            "widget/indicator `{id}` refresh hint is outside app_widgets_v1 bounds"
        )));
    }
    Ok(())
}

fn validate_widget_title(
    field: &'static str,
    value: &str,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    let declaration_ceiling = if field == "widget.actions.label" {
        APP_WIDGET_MAX_ACTION_LABEL_BYTES
    } else {
        APP_SURFACING_MAX_TITLE_BYTES
    };
    if value.trim().is_empty()
        || value.len() > limits.max_string_bytes.min(declaration_ceiling)
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(AppManifestError::InvalidManifest(format!(
            "{field} must be bounded non-control text"
        )));
    }
    Ok(())
}

fn widget_capability_for_view(
    kind: AppManifestViewKind,
) -> Option<AppManifestWidgetClientCapability> {
    match kind {
        AppManifestViewKind::List => Some(AppManifestWidgetClientCapability::DeclarativeListV1),
        AppManifestViewKind::Table => Some(AppManifestWidgetClientCapability::DeclarativeTableV1),
        AppManifestViewKind::Tree => Some(AppManifestWidgetClientCapability::DeclarativeTreeV1),
        AppManifestViewKind::Timeline => {
            Some(AppManifestWidgetClientCapability::DeclarativeTimelineV1)
        },
        AppManifestViewKind::Graph => Some(AppManifestWidgetClientCapability::DeclarativeGraphV1),
        AppManifestViewKind::Board => None,
    }
}

fn custom_surface_entry_exists(manifest: &AppPackageManifest, route: &AppRoute) -> bool {
    manifest
        .app
        .custom_surface
        .as_ref()
        .is_some_and(|declaration| {
            declaration
                .entry_points
                .iter()
                .any(|entry| &entry.route == route)
        })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestDataPolicy {
    pub defaults: AppManifestPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestPolicy {
    pub classification_floor: AppDataClassification,
    pub model_processing: AppModelProcessing,
    pub personal_agent_access: AppPersonalAgentAccess,
    pub memory_promotion: AppMemoryPromotion,
    pub external_egress: AppExternalEgress,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_destinations: Vec<AppReference>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestPolicyOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification_floor: Option<AppDataClassification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_processing: Option<AppModelProcessing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personal_agent_access: Option<AppPersonalAgentAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_promotion: Option<AppMemoryPromotion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_egress: Option<AppExternalEgress>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_destinations: Vec<AppReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestEntity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_policy: Option<AppManifestPolicyOverride>,
    pub fields: BTreeMap<AppName, AppManifestField>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppReferenceDeletePolicy {
    Restrict,
    Nullify,
    Cascade,
}

impl Default for AppReferenceDeletePolicy {
    fn default() -> Self {
        Self::Restrict
    }
}

fn is_default_reference_delete_policy(value: &AppReferenceDeletePolicy) -> bool {
    *value == AppReferenceDeletePolicy::Restrict
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppReferenceCyclePolicy {
    Deny,
    AllowBounded,
}

impl Default for AppReferenceCyclePolicy {
    fn default() -> Self {
        Self::Deny
    }
}

fn is_default_reference_cycle_policy(value: &AppReferenceCyclePolicy) -> bool {
    *value == AppReferenceCyclePolicy::Deny
}

const fn default_relation_max_traversal_depth() -> u16 {
    1
}

fn is_default_relation_max_traversal_depth(value: &u16) -> bool {
    *value == default_relation_max_traversal_depth()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestField {
    Text {
        #[serde(default)]
        required: bool,
        #[serde(default)]
        nullable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_policy: Option<AppManifestPolicyOverride>,
    },
    Markdown {
        #[serde(default)]
        required: bool,
        #[serde(default)]
        nullable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_policy: Option<AppManifestPolicyOverride>,
    },
    Integer {
        #[serde(default)]
        required: bool,
        #[serde(default)]
        nullable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_policy: Option<AppManifestPolicyOverride>,
    },
    Decimal {
        #[serde(default)]
        required: bool,
        #[serde(default)]
        nullable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_policy: Option<AppManifestPolicyOverride>,
    },
    Boolean {
        #[serde(default)]
        required: bool,
        #[serde(default)]
        nullable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_policy: Option<AppManifestPolicyOverride>,
    },
    Timestamp {
        #[serde(default)]
        required: bool,
        #[serde(default)]
        nullable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_policy: Option<AppManifestPolicyOverride>,
    },
    Enum {
        values: Vec<AppName>,
        #[serde(default)]
        required: bool,
        #[serde(default)]
        nullable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_policy: Option<AppManifestPolicyOverride>,
    },
    Reference {
        entity: AppName,
        #[serde(default)]
        required: bool,
        #[serde(default)]
        nullable: bool,
        #[serde(default, skip_serializing_if = "is_default_reference_delete_policy")]
        on_delete: AppReferenceDeletePolicy,
        #[serde(default, skip_serializing_if = "is_default_reference_cycle_policy")]
        cycle_policy: AppReferenceCyclePolicy,
        #[serde(
            default = "default_relation_max_traversal_depth",
            skip_serializing_if = "is_default_relation_max_traversal_depth"
        )]
        max_traversal_depth: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_policy: Option<AppManifestPolicyOverride>,
    },
}

impl AppManifestField {
    pub(crate) fn policy(&self) -> Option<&AppManifestPolicyOverride> {
        match self {
            Self::Text { data_policy, .. }
            | Self::Markdown { data_policy, .. }
            | Self::Integer { data_policy, .. }
            | Self::Decimal { data_policy, .. }
            | Self::Boolean { data_policy, .. }
            | Self::Timestamp { data_policy, .. }
            | Self::Enum { data_policy, .. }
            | Self::Reference { data_policy, .. } => data_policy.as_ref(),
        }
    }

    fn required(&self) -> bool {
        match self {
            Self::Text { required, .. }
            | Self::Markdown { required, .. }
            | Self::Integer { required, .. }
            | Self::Decimal { required, .. }
            | Self::Boolean { required, .. }
            | Self::Timestamp { required, .. }
            | Self::Enum { required, .. }
            | Self::Reference { required, .. } => *required,
        }
    }

    fn nullable(&self) -> bool {
        match self {
            Self::Text { nullable, .. }
            | Self::Markdown { nullable, .. }
            | Self::Integer { nullable, .. }
            | Self::Decimal { nullable, .. }
            | Self::Boolean { nullable, .. }
            | Self::Timestamp { nullable, .. }
            | Self::Enum { nullable, .. }
            | Self::Reference { nullable, .. } => *nullable,
        }
    }

    fn accepts_json_value(&self, value: &serde_json::Value) -> bool {
        if value.is_null() {
            return self.nullable();
        }
        match self {
            Self::Text { .. } | Self::Markdown { .. } | Self::Reference { .. } => value.is_string(),
            Self::Integer { .. } => value.as_i64().is_some() || value.as_u64().is_some(),
            Self::Decimal { .. } => value.is_number(),
            Self::Boolean { .. } => value.is_boolean(),
            Self::Timestamp { .. } => value
                .as_str()
                .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
                .is_some(),
            Self::Enum { values, .. } => value
                .as_str()
                .is_some_and(|raw| values.iter().any(|candidate| candidate.as_str() == raw)),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestViewKind {
    List,
    Table,
    Tree,
    Timeline,
    /// Entity-backed node/edge graph over the MUIJ `Graph` family (plan 1.5
    /// completion): nodes are records, node labels come from a non-structural
    /// text or markdown field, and one nullable self-reference field derives
    /// the parent→child edges. Read-only — the family's bounded interactions
    /// (select/expand/focus/reveal) carry no mutations.
    Graph,
    /// Parsed only so conformance can return the stable, explicit V1
    /// unsupported-view result instead of an opaque Serde/YAML failure.
    Board,
}

/// Closed declarative component set for a default app surface. Package
/// authors select only admitted entity fields; the surface compiler owns all
/// record, query, pagination and mutation bindings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "component", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppManifestSurfaceComponent {
    Detail {
        id: AppName,
        fields: Vec<AppName>,
    },
    Form {
        id: AppName,
        fields: Vec<AppName>,
    },
    Section {
        id: AppName,
        children: Vec<AppManifestSurfaceComponent>,
    },
    List {
        id: AppName,
        fields: Vec<AppName>,
    },
    Table {
        id: AppName,
        columns: Vec<AppName>,
    },
}

pub const APP_SURFACE_MAX_DECLARATIVE_COMPONENTS: usize = 64;
pub const APP_SURFACE_MAX_DECLARATIVE_DEPTH: usize = 8;
pub const APP_SURFACE_MAX_DECLARATIVE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestView {
    pub entity: AppName,
    pub kind: AppManifestViewKind,
    pub route: AppRoute,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<AppManifestSurfaceComponent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_field: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_field: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_field: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_field: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_field: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_field: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_field: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_field: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_field: Option<AppName>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestRunner {
    Auto,
    Recipe,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestTrigger {
    User,
    Schedule,
    Event,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestSchemaType {
    Object,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestInputSchema {
    #[serde(rename = "type")]
    pub schema_type: AppManifestSchemaType,
    pub fields: BTreeMap<AppName, AppManifestField>,
    /// Optional canonical bounded workflow-value graph. Legacy scalar-object
    /// manifests omit this member and retain byte-for-byte manifest identity;
    /// their fields are deterministically projected into the same V1 graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_schema: Option<AppWorkflowValueSchemaSource>,
}

impl AppManifestInputSchema {
    pub fn compiled_value_schema(
        &self,
    ) -> Result<AppCompiledWorkflowValueSchema, AppManifestError> {
        let source = self
            .value_schema
            .clone()
            .unwrap_or_else(|| self.legacy_value_schema_source());
        compile_workflow_value_schema(source).map_err(|error| {
            AppManifestError::InvalidManifest(format!("workflow value schema is invalid: {error}"))
        })
    }

    pub fn value_schema_digest(&self) -> Result<AppDigest, AppManifestError> {
        Ok(self.compiled_value_schema()?.content_digest().clone())
    }

    pub(crate) fn root_value_type(&self) -> Result<AppWorkflowValueTypeNode, AppManifestError> {
        Ok(self.compiled_value_schema()?.root_node().clone())
    }

    fn legacy_value_schema_source(&self) -> AppWorkflowValueSchemaSource {
        let mut nodes = Vec::with_capacity(self.fields.len().saturating_mul(2).saturating_add(1));
        let mut fields = BTreeMap::new();
        for (name, field) in &self.fields {
            let scalar = match field {
                AppManifestField::Text { .. } => {
                    AppWorkflowValueTypeNode::Text { max_bytes: 262_144 }
                },
                AppManifestField::Markdown { .. } => {
                    AppWorkflowValueTypeNode::Markdown { max_bytes: 262_144 }
                },
                AppManifestField::Reference { entity, .. } => {
                    AppWorkflowValueTypeNode::EntityReference {
                        entity: entity.clone(),
                    }
                },
                AppManifestField::Integer { .. } => AppWorkflowValueTypeNode::Integer,
                AppManifestField::Decimal { .. } => AppWorkflowValueTypeNode::Decimal,
                AppManifestField::Boolean { .. } => AppWorkflowValueTypeNode::Boolean,
                AppManifestField::Timestamp { .. } => AppWorkflowValueTypeNode::Timestamp,
                AppManifestField::Enum { values, .. } => AppWorkflowValueTypeNode::Enum {
                    values: values.iter().cloned().collect(),
                },
            };
            let scalar_index = u16::try_from(nodes.len()).expect("manifest field limit fits u16");
            nodes.push(scalar);
            let value_type = if field.nullable() {
                let nullable_index =
                    u16::try_from(nodes.len()).expect("manifest field limit fits u16");
                nodes.push(AppWorkflowValueTypeNode::Nullable {
                    value_type: scalar_index,
                });
                nullable_index
            } else {
                scalar_index
            };
            fields.insert(
                name.clone(),
                AppWorkflowRecordField {
                    value_type,
                    required: field.required(),
                },
            );
        }
        let root = u16::try_from(nodes.len()).expect("manifest field limit fits u16");
        nodes.push(AppWorkflowValueTypeNode::Record { fields });
        AppWorkflowValueSchemaSource {
            version: AppWorkflowValueSchemaVersion::V1,
            root,
            handling_floor: AppWorkflowHandlingFloor {
                classification: AppDataClassification::Public,
                model_processing: AppModelProcessing::RemoteAllowed,
            },
            nodes,
        }
    }

    /// Project the closed app object schema into the JSON Schema vocabulary
    /// used by primitive descriptors/tool emission. This is a deterministic
    /// projection of the already-parsed manifest types; it does not accept an
    /// arbitrary caller-supplied schema or loosen unknown-field rejection.
    pub fn to_primitive_json_schema(&self) -> serde_json::Value {
        if self.value_schema.is_some() {
            return self
                .compiled_value_schema()
                .ok()
                .and_then(|schema| workflow_value_json_schema(&schema, schema.source().root, 0))
                .unwrap_or_else(|| serde_json::json!({ "not": {} }));
        }
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        for (name, field) in &self.fields {
            let (kind, format, values) = match field {
                AppManifestField::Text { .. }
                | AppManifestField::Markdown { .. }
                | AppManifestField::Reference { .. } => ("string", None, None),
                AppManifestField::Integer { .. } => ("integer", None, None),
                AppManifestField::Decimal { .. } => ("number", None, None),
                AppManifestField::Boolean { .. } => ("boolean", None, None),
                AppManifestField::Timestamp { .. } => ("string", Some("date-time"), None),
                AppManifestField::Enum { values, .. } => (
                    "string",
                    None,
                    Some(
                        values
                            .iter()
                            .map(|value| serde_json::Value::String(value.as_str().to_owned()))
                            .collect::<Vec<_>>(),
                    ),
                ),
            };
            let mut property = serde_json::Map::new();
            property.insert(
                "type".to_owned(),
                if field.nullable() {
                    serde_json::json!([kind, "null"])
                } else {
                    serde_json::Value::String(kind.to_owned())
                },
            );
            if let Some(format) = format {
                property.insert(
                    "format".to_owned(),
                    serde_json::Value::String(format.to_owned()),
                );
            }
            if let Some(values) = values {
                let mut values = values;
                if field.nullable() {
                    values.push(serde_json::Value::Null);
                }
                property.insert("enum".to_owned(), serde_json::Value::Array(values));
            }
            properties.insert(
                name.as_str().to_owned(),
                serde_json::Value::Object(property),
            );
            if field.required() {
                required.push(serde_json::Value::String(name.as_str().to_owned()));
            }
        }
        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }

    /// Validate a workflow invocation against the package-declared object
    /// schema. Unknown fields fail closed; execution never treats arbitrary
    /// caller JSON as implicit workflow context.
    pub fn validate_value(&self, value: &serde_json::Value) -> Result<(), AppManifestError> {
        if self.value_schema.is_some() {
            let schema = self.compiled_value_schema()?;
            return validate_json_workflow_value_shape(&schema, value).map_err(|error| {
                AppManifestError::InvalidManifest(format!(
                    "workflow value does not match its declared schema: {error}"
                ))
            });
        }
        let object = value.as_object().ok_or_else(|| {
            AppManifestError::InvalidManifest(
                "workflow input must be an object matching its declared schema".to_string(),
            )
        })?;
        for key in object.keys() {
            let name = AppName::parse(key.clone()).map_err(|error| {
                AppManifestError::InvalidManifest(format!(
                    "workflow input field `{key}` is invalid: {error}"
                ))
            })?;
            if !self.fields.contains_key(&name) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow input contains undeclared field `{key}`"
                )));
            }
        }
        for (name, field) in &self.fields {
            match object.get(name.as_str()) {
                None if field.required() => {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "workflow input is missing required field `{name}`"
                    )))
                },
                Some(value) if !field.accepts_json_value(value) => {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "workflow input field `{name}` does not match its declared type"
                    )))
                },
                _ => {},
            }
        }
        Ok(())
    }

    /// Validate a server-produced action result. Resource-bearing roots use
    /// opaque public handles and never accept Artifact task/execution/storage
    /// identifiers as substitutions.
    pub fn validate_result_value(&self, value: &serde_json::Value) -> Result<(), AppManifestError> {
        let root = self.root_value_type()?;
        let expected_prefix = match root {
            AppWorkflowValueTypeNode::EntityProjectionRef { .. } => Some("entity-handle:"),
            AppWorkflowValueTypeNode::ArtifactRef { .. } => Some("artifact-handle:"),
            AppWorkflowValueTypeNode::ReceiptRef { .. } => Some("receipt-handle:"),
            AppWorkflowValueTypeNode::ResourceRef { .. } => Some("resource-handle:"),
            _ => None,
        };
        if let Some(prefix) = expected_prefix {
            let handle = value.as_str().ok_or_else(|| {
                AppManifestError::InvalidManifest(
                    "resource result must be one opaque logical handle".to_owned(),
                )
            })?;
            let reference = AppReference::parse(handle.to_owned()).map_err(|error| {
                AppManifestError::InvalidManifest(format!(
                    "resource result handle is invalid: {error}"
                ))
            })?;
            if !reference.as_str().starts_with(prefix)
                || reference.as_str().contains("task")
                || reference.as_str().contains("execution")
            {
                return Err(AppManifestError::InvalidManifest(
                    "resource result substituted a non-public or internal identifier".to_owned(),
                ));
            }
            return Ok(());
        }
        self.validate_value(value)
    }

    pub(crate) fn present_result_fields(
        &self,
        value: &serde_json::Value,
    ) -> Result<Vec<AppFieldPath>, AppManifestError> {
        self.validate_result_value(value)?;
        let Some(object) = value.as_object() else {
            return Ok(Vec::new());
        };
        if self.value_schema.is_none() {
            return self
                .fields
                .keys()
                .filter(|name| object.contains_key(name.as_str()))
                .map(|name| {
                    AppFieldPath::parse(name.as_str()).map_err(|error| {
                        AppManifestError::InvalidManifest(format!(
                            "result field path is invalid: {error}"
                        ))
                    })
                })
                .collect();
        }
        let compiled = self.compiled_value_schema()?;
        let AppWorkflowValueTypeNode::Record { fields } = compiled.root_node() else {
            return Ok(Vec::new());
        };
        fields
            .keys()
            .filter(|name| object.contains_key(name.as_str()))
            .map(|name| {
                AppFieldPath::parse(name.as_str()).map_err(|error| {
                    AppManifestError::InvalidManifest(format!(
                        "result field path is invalid: {error}"
                    ))
                })
            })
            .collect()
    }

    /// Resolve the most restrictive package-declared handling floor for the
    /// fields actually present in one admitted workflow input. The caller
    /// supplies an already-resolved grant/agent/trust floor; wire labels never
    /// participate in this computation.
    pub fn resolve_handling_floor(
        &self,
        value: &serde_json::Value,
        mut classification: AppDataClassification,
        mut model_processing: AppModelProcessing,
    ) -> Result<(AppDataClassification, AppModelProcessing), AppManifestError> {
        self.validate_value(value)?;
        if let Some(source) = self.value_schema.as_ref() {
            classification = classification.max(source.handling_floor.classification);
            model_processing = model_processing.min(source.handling_floor.model_processing);
            return Ok((classification, model_processing));
        }
        let object = value.as_object().ok_or_else(|| {
            AppManifestError::InvalidManifest(
                "workflow input must be an object matching its declared schema".to_string(),
            )
        })?;
        for (name, field) in &self.fields {
            if !object.contains_key(name.as_str()) {
                continue;
            }
            if let Some(policy) = field.policy() {
                if let Some(floor) = policy.classification_floor {
                    classification = classification.max(floor);
                }
                if let Some(processing) = policy.model_processing {
                    model_processing = model_processing.min(processing);
                }
            }
        }
        Ok((classification, model_processing))
    }
}

fn workflow_value_json_schema(
    schema: &AppCompiledWorkflowValueSchema,
    index: u16,
    depth: usize,
) -> Option<serde_json::Value> {
    if depth > 32 {
        return None;
    }
    let node = schema.source().nodes.get(usize::from(index))?;
    Some(match node {
        AppWorkflowValueTypeNode::Unit => serde_json::json!({ "type": "null" }),
        AppWorkflowValueTypeNode::Boolean => serde_json::json!({ "type": "boolean" }),
        AppWorkflowValueTypeNode::Integer => serde_json::json!({ "type": "integer" }),
        AppWorkflowValueTypeNode::Decimal => serde_json::json!({ "type": "number" }),
        AppWorkflowValueTypeNode::Text { max_bytes }
        | AppWorkflowValueTypeNode::Markdown { max_bytes } => serde_json::json!({
            "type": "string",
            "maxLength": max_bytes,
        }),
        AppWorkflowValueTypeNode::Enum { values } => serde_json::json!({
            "type": "string",
            "enum": values,
        }),
        AppWorkflowValueTypeNode::Timestamp => serde_json::json!({
            "type": "string",
            "format": "date-time",
        }),
        AppWorkflowValueTypeNode::EntityReference { .. } => serde_json::json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 256,
        }),
        AppWorkflowValueTypeNode::OpaqueReference => serde_json::json!({
            "type": "string",
            "pattern": "^ref:",
        }),
        AppWorkflowValueTypeNode::EntityProjectionRef { .. } => serde_json::json!({
            "type": "string",
            "pattern": "^entity-handle:",
        }),
        AppWorkflowValueTypeNode::ArtifactRef { .. } => serde_json::json!({
            "type": "string",
            "pattern": "^artifact-handle:",
        }),
        AppWorkflowValueTypeNode::ReceiptRef { .. } => serde_json::json!({
            "type": "string",
            "pattern": "^receipt-handle:",
        }),
        AppWorkflowValueTypeNode::ResourceRef { .. } => serde_json::json!({
            "type": "string",
            "pattern": "^resource-handle:",
        }),
        AppWorkflowValueTypeNode::Nullable { value_type } => serde_json::json!({
            "anyOf": [
                workflow_value_json_schema(schema, *value_type, depth + 1)?,
                { "type": "null" }
            ]
        }),
        AppWorkflowValueTypeNode::Array {
            items,
            min_items,
            max_items,
        } => serde_json::json!({
            "type": "array",
            "items": workflow_value_json_schema(schema, *items, depth + 1)?,
            "minItems": min_items,
            "maxItems": max_items,
        }),
        AppWorkflowValueTypeNode::Record { fields } => {
            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();
            for (name, field) in fields {
                properties.insert(
                    name.to_string(),
                    workflow_value_json_schema(schema, field.value_type, depth + 1)?,
                );
                if field.required {
                    required.push(serde_json::Value::String(name.to_string()));
                }
            }
            serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            })
        },
        AppWorkflowValueTypeNode::TaggedUnion {
            discriminator,
            variants,
        } => {
            let alternatives = variants
                .iter()
                .map(|(tag, value_type)| {
                    workflow_value_json_schema(schema, *value_type, depth + 1).map(|value| {
                        let mut properties = serde_json::Map::new();
                        properties.insert(
                            discriminator.to_string(),
                            serde_json::json!({ "const": tag }),
                        );
                        properties.insert("value".to_owned(), value);
                        serde_json::json!({
                            "type": "object",
                            "properties": properties,
                            "required": [discriminator, "value"],
                            "additionalProperties": false,
                        })
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            serde_json::json!({ "oneOf": alternatives })
        },
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppManifestResultKind {
    EntityProjection,
    TypedValue,
    ArtifactReference,
    ReceiptReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestWorkflowResult {
    pub kind: AppManifestResultKind,
    pub entities: Vec<AppName>,
    /// Optional reviewed wire shape for the workflow's primary JSON output.
    ///
    /// Existing packages may omit this and remain executable, but an action
    /// result cannot be used as typed input to another app without it. This is
    /// intentionally the same closed scalar/object vocabulary used by workflow
    /// input: composition never infers a schema from model-produced JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<AppManifestInputSchema>,
}

/// One package-requested, workflow-local contribution port. The port owns no
/// destination state and cannot select arbitrary source material: its source
/// is the exact record in the declaring workflow's mutation-backed entity
/// projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestContributionPort {
    pub source: AppContributionSource,
    pub destination: AppContributionDestination,
    pub purposes: Vec<AppName>,
    pub audiences: Vec<AppReference>,
    pub evidence_classes: Vec<AppContributionEvidenceClass>,
    pub frequency: AppContributionFrequency,
    pub maximum_retention_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestWorkflow {
    pub prompt: AppBundlePath,
    pub runner: AppManifestRunner,
    /// Exact immutable recipe bundle member for `runner: recipe`. The member
    /// contains the closed IR plus its complete content-addressed schema
    /// catalog; package admission compiles it and binds its bytes into the
    /// package lock before the workflow can be selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<AppBundlePath>,
    /// Optional named agent that runs this workflow. Omitted workflows keep
    /// the platform default `personal-assistant`, which is requested on the
    /// grant so the owner can deny it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AppName>,
    /// Optional personality selected for that runner. This is delegation,
    /// not a Pack tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<AppName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<AppName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub procedures: Vec<AppReference>,
    pub input: AppManifestInputSchema,
    pub result: AppManifestWorkflowResult,
    /// Closed proposal ports reviewed at installation. Omission means this
    /// workflow has no memory/retrieval proposal capability.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contribution_ports: BTreeMap<AppName, AppManifestContributionPort>,
    /// Exact owner-notification effects this workflow may request. Omission is
    /// deny-all and preserves legacy workflow identity.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notification_ports: BTreeMap<AppName, AppManifestNotificationPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub may_mutate: Vec<AppName>,
    pub trigger: AppManifestTrigger,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestAction {
    pub workflow: AppName,
    pub input_from: AppReference,
    pub result_from: AppReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestResources {
    pub per_run: AppManifestRunResources,
    pub monthly: AppManifestMonthlyResources,
    pub storage: AppManifestStorageResources,
    /// Exact per-behavior ceilings. Under `app_behaviors_v1` its key set must
    /// equal the declared behavior ids; omission preserves legacy manifests.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub behaviors: BTreeMap<AppName, AppManifestBehaviorResources>,
    /// Exact per-event-behavior ceilings. Its key set must equal the additive
    /// event behavior declaration ids.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub event_behaviors: BTreeMap<AppName, AppManifestBehaviorResources>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestBehaviorResources {
    pub per_run: AppManifestRunResources,
    pub monthly: AppManifestMonthlyResources,
    pub max_starts_per_period: u32,
    pub period_seconds: u64,
    pub max_causation_depth: u16,
    pub max_spend_depth: u16,
    pub max_contribution_proposals_per_run: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestRunResources {
    pub max_tokens: u64,
    pub max_cost_usd: AppUsdAmount,
    pub max_active_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestMonthlyResources {
    pub max_tokens: u64,
    pub max_cost_usd: AppUsdAmount,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestStorageResources {
    pub max_records: u64,
    pub max_bytes: u64,
}

/// Exact monetary amount represented as micro-USD. YAML accepts a plain
/// non-negative decimal with at most six fractional digits; canonical JSON
/// serializes the explicit integer unit.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct AppUsdAmount(u64);

impl AppUsdAmount {
    pub fn microusd(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for AppUsdAmount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        let raw = match value {
            serde_yaml::Value::Number(number) => number.to_string(),
            serde_yaml::Value::String(text) => text,
            _ => {
                return Err(de::Error::custom(
                    "USD amount must be a non-negative decimal",
                ))
            },
        };
        parse_usd_micros(&raw).map(Self).map_err(de::Error::custom)
    }
}

fn parse_usd_micros(raw: &str) -> Result<u64, &'static str> {
    if raw.is_empty()
        || raw.starts_with('-')
        || raw.bytes().any(|byte| matches!(byte, b'e' | b'E' | b'+'))
    {
        return Err("USD amount must be a non-negative plain decimal");
    }
    let mut parts = raw.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("USD amount must be a non-negative plain decimal");
    }
    let whole = whole
        .parse::<u64>()
        .map_err(|_| "USD amount is too large")?;
    let mut micros = whole
        .checked_mul(1_000_000)
        .ok_or("USD amount is too large")?;
    if let Some(fraction) = fraction {
        if fraction.is_empty()
            || fraction.len() > 6
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("USD amount supports at most six fractional digits");
        }
        let fractional = fraction
            .parse::<u64>()
            .map_err(|_| "USD amount is invalid")?;
        let scale = 10u64.pow(u32::try_from(6usize.saturating_sub(fraction.len())).unwrap_or(0));
        micros = micros
            .checked_add(fractional.saturating_mul(scale))
            .ok_or("USD amount is too large")?;
    }
    Ok(micros)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestDependencies {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub procedure_skills: Vec<AppManifestProcedureSkillDependency>,
    /// Canonical authoring field: real tool names, not `capability:` wrappers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<AppManifestToolDependency>,
    /// One-release read of the previous `capabilities` authoring field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<AppManifestCapabilityDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestProcedureSkillDependency {
    pub skill: AppReference,
    pub version_requirement: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendored_path: Option<AppBundlePath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestToolDependency {
    pub name: AppName,
    /// Optional exact internal primitive selector. Friendly names remain the
    /// workflow-local binding, while colliding private sources must select one
    /// content-addressed descriptor explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive_ref: Option<AppReference>,
    /// Optional reviewed action subset. Each selector is either one exact
    /// action name or its content-addressed action ref. Empty preserves the
    /// legacy "all descriptor actions" meaning, which installation review
    /// must surface explicitly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    /// Complete interactive authority requested for this exact dependency.
    /// It is ignored for no owner: a non-interactive descriptor carrying this
    /// field is rejected while resolving the immutable package lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactive: Option<AppInteractiveCapabilityRequest>,
    pub version_requirement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppManifestCapabilityDependency {
    pub capability: AppName,
    pub version_requirement: String,
}

impl AppManifestDependencies {
    /// Canonical declared tools. `tools:` wins. `capabilities:` remains a
    /// one-release alias. Both non-empty lists fail closed.
    pub fn declared_tools(&self) -> Result<Vec<AppManifestToolDependency>, AppManifestError> {
        if !self.tools.is_empty() && !self.capabilities.is_empty() {
            return Err(AppManifestError::InvalidManifest(
                "declare dependencies.tools or the one-release dependencies.capabilities alias, \
                 not both"
                    .to_owned(),
            ));
        }
        if !self.tools.is_empty() {
            return Ok(self.tools.clone());
        }
        Ok(self
            .capabilities
            .iter()
            .map(|dependency| AppManifestToolDependency {
                name: dependency.capability.clone(),
                primitive_ref: None,
                actions: Vec::new(),
                interactive: None,
                version_requirement: dependency.version_requirement.clone(),
            })
            .collect())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppDependencyKind {
    Contract,
    ProcedureSkill,
    Capability,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDependencyRequirement {
    pub kind: AppDependencyKind,
    pub dependency_ref: AppReference,
    pub version_requirement: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendored_path: Option<AppBundlePath>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalAppManifest {
    manifest: AppPackageManifest,
    manifest_digest: AppDigest,
}

impl CanonicalAppManifest {
    pub fn manifest(&self) -> &AppPackageManifest {
        &self.manifest
    }

    pub fn manifest_digest(&self) -> &AppDigest {
        &self.manifest_digest
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct AppBundlePath(String);

impl AppBundlePath {
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, AppManifestError> {
        normalize_bundle_path(raw.as_ref(), ABSOLUTE_MAX_BUNDLE_PATH_BYTES).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn collision_key(&self) -> String {
        normalized_collision_key(&self.0)
    }
}

impl TryFrom<String> for AppBundlePath {
    type Error = AppManifestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for AppBundlePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .try_into()
            .map_err(de::Error::custom)
    }
}

impl fmt::Display for AppBundlePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct AppRoute(String);

impl AppRoute {
    pub fn parse(raw: impl AsRef<str>) -> Result<Self, AppManifestError> {
        normalize_route(raw.as_ref()).map(Self)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parameter_names(&self) -> impl Iterator<Item = &str> {
        self.0
            .split('/')
            .filter_map(|segment| segment.strip_prefix(':'))
    }

    fn collision_key(&self) -> String {
        normalized_collision_key(&self.0)
    }
}

impl<'de> Deserialize<'de> for AppRoute {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppBundleMemberKind {
    RegularFile,
    Symlink,
    Special,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AppBundleMember {
    pub path: AppBundlePath,
    pub kind: AppBundleMemberKind,
    pub bytes: Vec<u8>,
}

impl AppBundleMember {
    pub fn regular_file(path: impl AsRef<str>, bytes: Vec<u8>) -> Result<Self, AppManifestError> {
        Ok(Self {
            path: AppBundlePath::parse(path)?,
            kind: AppBundleMemberKind::RegularFile,
            bytes,
        })
    }

    pub fn symlink(path: impl AsRef<str>) -> Result<Self, AppManifestError> {
        Ok(Self {
            path: AppBundlePath::parse(path)?,
            kind: AppBundleMemberKind::Symlink,
            bytes: Vec::new(),
        })
    }

    pub fn special(path: impl AsRef<str>) -> Result<Self, AppManifestError> {
        Ok(Self {
            path: AppBundlePath::parse(path)?,
            kind: AppBundleMemberKind::Special,
            bytes: Vec::new(),
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppValidatedBundleMember {
    path: AppBundlePath,
    content_digest: AppDigest,
    byte_len: u64,
    #[serde(skip)]
    bytes: std::sync::Arc<[u8]>,
}

impl AppValidatedBundleMember {
    pub fn path(&self) -> &AppBundlePath {
        &self.path
    }

    pub fn content_digest(&self) -> &AppDigest {
        &self.content_digest
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPackageCandidate {
    manifest: CanonicalAppManifest,
    bundle_digest: AppDigest,
    members: Vec<AppValidatedBundleMember>,
}

/// Incremental hostile-input admission for archive/filesystem readers.
///
/// The reader is capped before `Vec<u8>` allocation can grow beyond the fixed
/// production profile. A filesystem adapter must still open files with
/// no-follow semantics and reject non-regular metadata before supplying the
/// reader here.
pub struct AppBundleStagingAdmission {
    limits: AppPackageLimits,
    members: Vec<AppBundleMember>,
    collision_keys: BTreeSet<String>,
    total_bytes: usize,
}

impl Default for AppBundleStagingAdmission {
    fn default() -> Self {
        Self {
            limits: AppPackageLimits::default(),
            members: Vec::new(),
            collision_keys: BTreeSet::new(),
            total_bytes: 0,
        }
    }
}

impl AppBundleStagingAdmission {
    pub fn push_regular_reader<R: Read>(
        &mut self,
        path: impl AsRef<str>,
        declared_byte_len: Option<u64>,
        mut reader: R,
    ) -> Result<(), AppManifestError> {
        if self.members.len() >= self.limits.max_bundle_files {
            return Err(AppManifestError::BundleFileLimit {
                limit: self.limits.max_bundle_files,
            });
        }
        let path = AppBundlePath::parse(path)?;
        if path.as_str().len() > self.limits.max_bundle_path_bytes {
            return Err(AppManifestError::UnsafeBundlePath(format!(
                "path exceeds {} bytes",
                self.limits.max_bundle_path_bytes
            )));
        }
        let collision_key = path.collision_key();
        if self.collision_keys.contains(&collision_key) {
            return Err(AppManifestError::DuplicateNormalizedPath(path.to_string()));
        }

        let remaining_total = self
            .limits
            .max_bundle_bytes
            .saturating_sub(self.total_bytes);
        let member_ceiling = self.limits.max_bundle_file_bytes.min(remaining_total);
        if declared_byte_len
            .is_some_and(|declared| declared > u64::try_from(member_ceiling).unwrap_or(u64::MAX))
        {
            return Err(if remaining_total < self.limits.max_bundle_file_bytes {
                AppManifestError::BundleTooLarge {
                    limit: self.limits.max_bundle_bytes,
                }
            } else {
                AppManifestError::BundleMemberTooLarge {
                    path: path.to_string(),
                    limit: self.limits.max_bundle_file_bytes,
                }
            });
        }

        let read_ceiling = member_ceiling.saturating_add(1);
        let mut bytes = Vec::with_capacity(
            declared_byte_len
                .and_then(|declared| usize::try_from(declared).ok())
                .unwrap_or(0)
                .min(member_ceiling),
        );
        reader
            .by_ref()
            .take(u64::try_from(read_ceiling).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(|error| AppManifestError::BundleRead {
                path: path.to_string(),
                message: error.to_string(),
            })?;
        if bytes.len() > member_ceiling {
            return Err(if remaining_total < self.limits.max_bundle_file_bytes {
                AppManifestError::BundleTooLarge {
                    limit: self.limits.max_bundle_bytes,
                }
            } else {
                AppManifestError::BundleMemberTooLarge {
                    path: path.to_string(),
                    limit: self.limits.max_bundle_file_bytes,
                }
            });
        }
        if let Some(declared) = declared_byte_len {
            if declared != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
                return Err(AppManifestError::BundleMemberLengthMismatch {
                    path: path.to_string(),
                    declared,
                    actual: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                });
            }
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        self.collision_keys.insert(collision_key);
        self.members.push(AppBundleMember {
            path,
            kind: AppBundleMemberKind::RegularFile,
            bytes,
        });
        Ok(())
    }

    pub fn finish(self) -> Result<AppPackageCandidate, AppManifestError> {
        build_app_package_candidate(self.members, &self.limits)
    }
}

impl AppPackageCandidate {
    pub fn manifest(&self) -> &CanonicalAppManifest {
        &self.manifest
    }

    pub fn bundle_digest(&self) -> &AppDigest {
        &self.bundle_digest
    }

    pub fn members(&self) -> &[AppValidatedBundleMember] {
        &self.members
    }

    pub fn member(&self, path: &AppBundlePath) -> Option<&AppValidatedBundleMember> {
        self.members.iter().find(|member| member.path == *path)
    }
}

pub fn parse_app_manifest_frontmatter(
    source: &[u8],
    limits: &AppPackageLimits,
) -> Result<CanonicalAppManifest, AppManifestError> {
    let yaml = extract_frontmatter(source, limits.max_manifest_bytes)?;
    parse_app_manifest_yaml(yaml, limits)
}

pub fn parse_app_manifest_yaml(
    yaml: &[u8],
    limits: &AppPackageLimits,
) -> Result<CanonicalAppManifest, AppManifestError> {
    preflight_manifest_yaml(yaml, limits)?;
    let yaml_value: serde_yaml::Value = serde_yaml::from_slice(yaml)
        .map_err(|error| AppManifestError::InvalidYaml(error.to_string()))?;
    validate_yaml_value_limits(&yaml_value, limits)?;
    let manifest: AppPackageManifest = serde_yaml::from_value(yaml_value)
        .map_err(|error| AppManifestError::InvalidYaml(error.to_string()))?;
    validate_manifest(&manifest, limits)?;
    let canonical_value = serde_json::to_value(&manifest)
        .map_err(|error| AppManifestError::CanonicalEncoding(error.to_string()))?;
    let canonical_bytes = canonical_json_bytes(&canonical_value)
        .map_err(|error| AppManifestError::CanonicalEncoding(error.to_string()))?;
    Ok(CanonicalAppManifest {
        manifest,
        manifest_digest: AppDigest::blake3(&canonical_bytes),
    })
}

pub fn build_app_package_candidate(
    members: Vec<AppBundleMember>,
    limits: &AppPackageLimits,
) -> Result<AppPackageCandidate, AppManifestError> {
    if members.is_empty() || members.len() > limits.max_bundle_files {
        return Err(AppManifestError::BundleFileLimit {
            limit: limits.max_bundle_files,
        });
    }
    let mut collision_keys = BTreeSet::new();
    let mut total_bytes = 0usize;
    let mut validated = Vec::with_capacity(members.len());
    for member in members {
        if member.path.as_str().len() > limits.max_bundle_path_bytes {
            return Err(AppManifestError::UnsafeBundlePath(format!(
                "path exceeds {} bytes",
                limits.max_bundle_path_bytes
            )));
        }
        if member.kind != AppBundleMemberKind::RegularFile {
            return Err(match member.kind {
                AppBundleMemberKind::Symlink => {
                    AppManifestError::SymlinkMember(member.path.to_string())
                },
                AppBundleMemberKind::Special => {
                    AppManifestError::SpecialMember(member.path.to_string())
                },
                AppBundleMemberKind::RegularFile => unreachable!(),
            });
        }
        if !collision_keys.insert(member.path.collision_key()) {
            return Err(AppManifestError::DuplicateNormalizedPath(
                member.path.to_string(),
            ));
        }
        if member.bytes.len() > limits.max_bundle_file_bytes {
            return Err(AppManifestError::BundleMemberTooLarge {
                path: member.path.to_string(),
                limit: limits.max_bundle_file_bytes,
            });
        }
        total_bytes = total_bytes.saturating_add(member.bytes.len());
        if total_bytes > limits.max_bundle_bytes {
            return Err(AppManifestError::BundleTooLarge {
                limit: limits.max_bundle_bytes,
            });
        }
        validated.push(AppValidatedBundleMember {
            content_digest: AppDigest::blake3(&member.bytes),
            byte_len: u64::try_from(member.bytes.len()).unwrap_or(u64::MAX),
            path: member.path,
            bytes: member.bytes.into(),
        });
    }
    validated.sort_by(|left, right| left.path.collision_key().cmp(&right.path.collision_key()));
    let skill_path = AppBundlePath::parse("SKILL.md")?;
    let skill = validated
        .iter()
        .find(|member| member.path == skill_path)
        .ok_or(AppManifestError::MissingSkillManifest)?;
    let manifest = parse_app_manifest_frontmatter(&skill.bytes, limits)?;
    ensure_referenced_members_exist(&manifest.manifest, &validated)?;
    validate_custom_surface_bundle_members(&manifest.manifest, &validated)?;

    #[derive(Serialize)]
    struct BundleIdentity<'a> {
        version: u8,
        manifest_digest: &'a AppDigest,
        members: Vec<BundleMemberIdentity<'a>>,
    }
    #[derive(Serialize)]
    struct BundleMemberIdentity<'a> {
        path: &'a AppBundlePath,
        content_digest: &'a AppDigest,
        byte_len: u64,
    }
    let identity = BundleIdentity {
        version: 1,
        manifest_digest: manifest.manifest_digest(),
        members: validated
            .iter()
            .map(|member| BundleMemberIdentity {
                path: &member.path,
                content_digest: &member.content_digest,
                byte_len: member.byte_len,
            })
            .collect(),
    };
    let identity_bytes = serde_json::to_vec(&identity)
        .map_err(|error| AppManifestError::CanonicalEncoding(error.to_string()))?;
    Ok(AppPackageCandidate {
        manifest,
        bundle_digest: AppDigest::blake3(&identity_bytes),
        members: validated,
    })
}

impl AppPackageManifest {
    pub fn declared_tools(&self) -> Result<Vec<AppManifestToolDependency>, AppManifestError> {
        self.app.dependencies.declared_tools()
    }

    pub fn declared_agents(&self) -> BTreeSet<AppName> {
        self.app
            .workflows
            .values()
            .filter_map(|workflow| workflow.agent.clone())
            .collect()
    }

    pub fn declared_personalities(&self) -> BTreeSet<AppName> {
        self.app
            .workflows
            .values()
            .filter_map(|workflow| workflow.personality.clone())
            .collect()
    }

    pub fn dependency_requirements(
        &self,
        limits: &AppPackageLimits,
    ) -> Result<Vec<AppDependencyRequirement>, AppManifestError> {
        let mut requirements = Vec::new();
        for (contract, version_requirement) in &self.app.compatibility {
            requirements.push(AppDependencyRequirement {
                kind: AppDependencyKind::Contract,
                dependency_ref: AppReference::parse(format!("contract:{contract}"))
                    .map_err(|error| AppManifestError::InvalidManifest(error.to_string()))?,
                version_requirement: version_requirement.clone(),
                vendored_path: None,
            });
        }
        for skill in &self.app.dependencies.procedure_skills {
            requirements.push(AppDependencyRequirement {
                kind: AppDependencyKind::ProcedureSkill,
                dependency_ref: skill.skill.clone(),
                version_requirement: skill.version_requirement.clone(),
                vendored_path: skill.vendored_path.clone(),
            });
        }
        let declared_tools = self.declared_tools()?;
        let declared_by_key = declared_tools
            .iter()
            .map(|dependency| {
                (
                    normalized_collision_key(dependency.name.as_str()),
                    dependency,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut used_tools = BTreeMap::<String, &AppName>::new();
        for tool in self
            .app
            .workflows
            .values()
            .flat_map(|workflow| &workflow.uses)
        {
            let key = normalized_collision_key(tool.as_str());
            if used_tools
                .insert(key, tool)
                .is_some_and(|existing| existing != tool)
            {
                return Err(AppManifestError::DuplicateNormalizedIdentifier(
                    tool.to_string(),
                ));
            }
        }
        for (key, used_tool) in used_tools {
            let declared = declared_by_key.get(&key).copied().ok_or_else(|| {
                AppManifestError::InvalidManifest(format!(
                    "workflow uses undeclared tool `{used_tool}`"
                ))
            })?;
            requirements.push(AppDependencyRequirement {
                kind: AppDependencyKind::Capability,
                dependency_ref: AppReference::parse(format!("capability:{}", declared.name))
                    .map_err(|error| AppManifestError::InvalidManifest(error.to_string()))?,
                version_requirement: declared.version_requirement.clone(),
                vendored_path: None,
            });
        }
        requirements.sort_by(|left, right| {
            (left.kind, &left.dependency_ref).cmp(&(right.kind, &right.dependency_ref))
        });
        if requirements.len() > limits.max_dependencies {
            return Err(AppManifestError::DependencyLimit {
                limit: limits.max_dependencies,
            });
        }
        Ok(requirements)
    }
}

fn validate_manifest(
    manifest: &AppPackageManifest,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    if manifest.description.is_empty() || manifest.description.len() > limits.max_string_bytes {
        return Err(AppManifestError::InvalidManifest(format!(
            "description must contain between 1 and {} bytes",
            limits.max_string_bytes
        )));
    }
    if manifest.version.is_empty() || manifest.version.len() > limits.max_string_bytes {
        return Err(AppManifestError::InvalidManifest(
            "version is empty or exceeds the string ceiling".to_owned(),
        ));
    }
    semver::Version::parse(&manifest.version)
        .map_err(|error| AppManifestError::InvalidManifest(format!("invalid version: {error}")))?;
    if manifest.metadata.magician.app_manifest_version != APP_MANIFEST_SCHEMA_VERSION {
        return Err(AppManifestError::UnsupportedManifestVersion(
            manifest.metadata.magician.app_manifest_version.clone(),
        ));
    }
    let magician = &manifest.metadata.magician;
    if magician.app_sdk_version.is_empty()
        || magician.app_sdk_version.len() > limits.max_string_bytes
    {
        return Err(AppManifestError::InvalidManifest(
            "metadata.magician.app_sdk_version is informational but must be a bounded non-empty \
             string"
                .to_owned(),
        ));
    }
    let mut required_features = BTreeSet::new();
    for feature in &magician.required_features {
        if !AppManifestFeature::supported().contains(feature) {
            return Err(AppManifestError::UnsupportedManifestFeature(format!(
                "{feature:?}"
            )));
        }
        if !required_features.insert(*feature) {
            return Err(AppManifestError::InvalidManifest(
                "metadata.magician.required_features contains duplicates".to_owned(),
            ));
        }
    }
    if let Some(generated_by) = &magician.generated_by {
        AppName::parse(generated_by.sdk.clone()).map_err(|error| {
            AppManifestError::InvalidManifest(format!(
                "metadata.magician.generated_by.sdk is invalid: {error}"
            ))
        })?;
        if generated_by.version.len() > limits.max_string_bytes {
            return Err(AppManifestError::InvalidManifest(
                "metadata.magician.generated_by.version exceeds the string ceiling".to_owned(),
            ));
        }
        semver::Version::parse(&generated_by.version).map_err(|error| {
            AppManifestError::InvalidManifest(format!(
                "metadata.magician.generated_by.version is invalid: {error}"
            ))
        })?;
    }
    validate_llm_operations(manifest)?;
    validate_memory_access(manifest)?;
    validate_custom_surface_declaration(manifest)?;
    validate_widget_declarations(manifest, limits)?;
    validate_named_map(
        "compatibility",
        &manifest.app.compatibility,
        limits.max_dependencies,
    )?;
    if manifest.app.compatibility.is_empty() {
        return Err(AppManifestError::InvalidManifest(
            "compatibility must not be empty".to_owned(),
        ));
    }
    for requirement in manifest.app.compatibility.values() {
        validate_version_requirement(requirement, limits)?;
    }
    validate_policy(&manifest.app.data_policy.defaults, limits)?;
    validate_named_map("entities", &manifest.app.entities, limits.max_entities)?;
    validate_named_map("views", &manifest.app.views, limits.max_views)?;
    validate_named_map("workflows", &manifest.app.workflows, limits.max_workflows)?;
    validate_named_map("actions", &manifest.app.actions, limits.max_actions)?;
    if manifest.app.entities.is_empty() {
        return Err(AppManifestError::InvalidManifest(
            "entities must not be empty".to_owned(),
        ));
    }
    if manifest.app.views.is_empty() {
        return Err(AppManifestError::InvalidManifest(
            "views must not be empty for an installable V1 app".to_owned(),
        ));
    }

    let mut total_fields = 0usize;
    let mut total_contribution_ports = 0usize;
    for (entity_name, entity) in &manifest.app.entities {
        if let Some(policy) = &entity.data_policy {
            validate_policy_override(policy, limits)?;
        }
        validate_named_map("entity.fields", &entity.fields, limits.max_fields)?;
        if entity.fields.is_empty() {
            return Err(AppManifestError::InvalidManifest(format!(
                "entity `{entity_name}` has no fields"
            )));
        }
        total_fields = total_fields.saturating_add(entity.fields.len());
        if total_fields > limits.max_fields {
            return Err(AppManifestError::FieldLimit {
                limit: limits.max_fields,
            });
        }
        for field in entity.fields.values() {
            validate_field(field, &manifest.app.entities, limits)?;
        }
    }

    let mut route_keys = BTreeSet::new();
    let mut admitted_routes = Vec::with_capacity(manifest.app.views.len());
    for view in manifest.app.views.values() {
        if !route_keys.insert(view.route.collision_key()) {
            return Err(AppManifestError::DuplicateRoute(
                view.route.as_str().to_owned(),
            ));
        }
        if admitted_routes.iter().any(|existing: &&AppRoute| {
            app_route_templates_overlap(existing.as_str(), view.route.as_str())
        }) {
            return Err(AppManifestError::DuplicateRoute(
                view.route.as_str().to_owned(),
            ));
        }
        admitted_routes.push(&view.route);
    }
    for (view_name, view) in &manifest.app.views {
        let entity = manifest.app.entities.get(&view.entity).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "view `{view_name}` references missing entity `{}`",
                view.entity
            ))
        })?;
        validate_view(view_name, view, entity, limits)?;
    }

    let declared_skills = validate_declared_dependencies(manifest, limits)?;
    for (workflow_name, workflow) in &manifest.app.workflows {
        total_contribution_ports =
            total_contribution_ports.saturating_add(workflow.contribution_ports.len());
        if total_contribution_ports > AppContractLimits::default().max_collection_items() {
            return Err(AppManifestError::CollectionLimit {
                field: "workflow.contribution_ports",
                limit: AppContractLimits::default().max_collection_items(),
            });
        }
        if workflow.input.value_schema.is_some() && !workflow.input.fields.is_empty() {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` input must use either legacy fields or one canonical value_schema, not both"
            )));
        }
        if workflow.input.fields.len() > limits.max_fields {
            return Err(AppManifestError::FieldLimit {
                limit: limits.max_fields,
            });
        }
        validate_named_map(
            "workflow.input.fields",
            &workflow.input.fields,
            limits.max_fields,
        )?;
        total_fields = total_fields.saturating_add(workflow.input.fields.len());
        if total_fields > limits.max_fields {
            return Err(AppManifestError::FieldLimit {
                limit: limits.max_fields,
            });
        }
        for field in workflow.input.fields.values() {
            validate_field(field, &manifest.app.entities, limits)?;
        }
        let compiled_input = workflow.input.compiled_value_schema()?;
        let input_root_is_closed_object = matches!(
            compiled_input.root_node(),
            AppWorkflowValueTypeNode::Record { .. }
        ) || matches!(
            (workflow.runner, compiled_input.root_node()),
            (
                AppManifestRunner::Recipe,
                AppWorkflowValueTypeNode::TaggedUnion { .. }
            )
        );
        if !input_root_is_closed_object {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` input value_schema root must be one closed record, or one closed tagged union for a recipe"
            )));
        }
        if let Some(output_schema) = workflow.result.output_schema.as_ref() {
            let output_field_limit = limits
                .max_fields
                .min(AppContractLimits::default().max_collection_items());
            if output_schema.value_schema.is_some() && !output_schema.fields.is_empty() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` result must use either legacy fields or one canonical value_schema, not both"
                )));
            }
            if output_schema.fields.is_empty() && output_schema.value_schema.is_none() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` result output_schema must declare at least one \
                     field"
                )));
            }
            if output_schema.fields.len() > output_field_limit {
                return Err(AppManifestError::FieldLimit {
                    limit: output_field_limit,
                });
            }
            validate_named_map(
                "workflow.result.output_schema.fields",
                &output_schema.fields,
                output_field_limit,
            )?;
            total_fields = total_fields.saturating_add(output_schema.fields.len());
            if total_fields > limits.max_fields {
                return Err(AppManifestError::FieldLimit {
                    limit: limits.max_fields,
                });
            }
            for field in output_schema.fields.values() {
                validate_field(field, &manifest.app.entities, limits)?;
            }
            let compiled_output = output_schema.compiled_value_schema()?;
            let kind_matches = match workflow.result.kind {
                AppManifestResultKind::EntityProjection => matches!(
                    compiled_output.root_node(),
                    AppWorkflowValueTypeNode::Record { .. }
                        | AppWorkflowValueTypeNode::EntityProjectionRef { .. }
                ),
                AppManifestResultKind::TypedValue => !matches!(
                    compiled_output.root_node(),
                    AppWorkflowValueTypeNode::Unit
                        | AppWorkflowValueTypeNode::EntityProjectionRef { .. }
                        | AppWorkflowValueTypeNode::ArtifactRef { .. }
                        | AppWorkflowValueTypeNode::ReceiptRef { .. }
                        | AppWorkflowValueTypeNode::ResourceRef { .. }
                ),
                AppManifestResultKind::ArtifactReference => matches!(
                    compiled_output.root_node(),
                    AppWorkflowValueTypeNode::ArtifactRef { .. }
                ),
                AppManifestResultKind::ReceiptReference => matches!(
                    compiled_output.root_node(),
                    AppWorkflowValueTypeNode::ReceiptRef { .. }
                ),
            };
            if !kind_matches {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` result kind does not match its canonical output_schema root"
                )));
            }
        } else if matches!(
            workflow.result.kind,
            AppManifestResultKind::ArtifactReference | AppManifestResultKind::ReceiptReference
        ) {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` artifact/receipt result requires an exact canonical output_schema"
            )));
        }
        validate_workflow_contribution_ports(
            workflow_name,
            workflow,
            &manifest.app.entities,
            &manifest.app.data_policy.defaults,
            magician
                .required_features
                .contains(&AppManifestFeature::ContributionPortsV1),
            limits,
        )?;
        validate_workflow_notification_ports(
            workflow_name,
            workflow,
            magician
                .required_features
                .contains(&AppManifestFeature::AppOwnerNotificationsV1),
        )?;
        validate_vec_limit(
            "workflow.uses",
            workflow.uses.len(),
            limits.max_dependencies,
        )?;
        validate_vec_limit(
            "workflow.procedures",
            workflow.procedures.len(),
            limits.max_dependencies,
        )?;
        validate_vec_limit(
            "workflow.result.entities",
            workflow.result.entities.len(),
            limits.max_entities,
        )?;
        validate_vec_limit(
            "workflow.may_mutate",
            workflow.may_mutate.len(),
            limits.max_entities,
        )?;
        match workflow.runner {
            AppManifestRunner::Auto => {
                let result_shape_valid = match workflow.result.kind {
                    AppManifestResultKind::EntityProjection => !workflow.result.entities.is_empty(),
                    AppManifestResultKind::TypedValue
                    | AppManifestResultKind::ArtifactReference
                    | AppManifestResultKind::ReceiptReference => {
                        workflow.result.entities.is_empty()
                            && workflow.result.output_schema.is_some()
                            && workflow.may_mutate.is_empty()
                    },
                };
                if workflow.recipe.is_some() || !result_shape_valid {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "workflow `{workflow_name}` auto runner result declaration is inconsistent or declares a recipe"
                    )));
                }
            },
            AppManifestRunner::Recipe => {
                if workflow.recipe.is_none()
                    || workflow.agent.is_some()
                    || workflow.personality.is_some()
                    || !workflow.procedures.is_empty()
                    || !workflow.contribution_ports.is_empty()
                    // Notification declarations narrow the ordinary terminal
                    // commit owner. Declaring one does not publish a proposal;
                    // native recipes still supply only host-built commit input.
                    || !matches!(
                        workflow.trigger,
                        AppManifestTrigger::User
                            | AppManifestTrigger::Schedule
                            | AppManifestTrigger::Event
                    )
                    || (workflow.trigger == AppManifestTrigger::Schedule
                        && !magician
                            .required_features
                            .contains(&AppManifestFeature::AppBehaviorsV1))
                    || (workflow.trigger == AppManifestTrigger::Event
                        && !magician
                            .required_features
                            .contains(&AppManifestFeature::AppEventBehaviorsV1))
                    || workflow.result.kind != AppManifestResultKind::TypedValue
                    || !workflow.result.entities.is_empty()
                    || workflow.result.output_schema.is_some()
                {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "workflow `{workflow_name}` recipe runner must be user-triggered or an \
                         exactly declared schedule/event behavior, typed-value-only and \
                         restricted to the admitted recipe node authority"
                    )));
                }
            },
        }
        validate_unique_names("workflow.uses", &workflow.uses)?;
        if let Some(tool) = workflow.uses.iter().find(|tool| {
            !crate::magician_v2::execution::compiled_dispatch::compiled_tool_is_exposable_on_surface(
                tool.as_str(),
                crate::magician_v2::execution::compiled_dispatch::GovernedAppToolSurface::AppWorkflow,
            )
        }) {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` cannot declare owner-facing governed tool `{tool}`"
            )));
        }
        validate_unique_refs("workflow.procedures", &workflow.procedures)?;
        validate_unique_names("workflow.result.entities", &workflow.result.entities)?;
        validate_unique_names("workflow.may_mutate", &workflow.may_mutate)?;
        for procedure in &workflow.procedures {
            if !declared_skills.contains(&normalized_collision_key(procedure.as_str())) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` uses undeclared procedure skill `{procedure}`"
                )));
            }
        }
        for entity in &workflow.result.entities {
            if !manifest.app.entities.contains_key(entity) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` returns missing entity `{entity}`"
                )));
            }
        }
        for entity in &workflow.may_mutate {
            if !manifest.app.entities.contains_key(entity) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` mutates missing entity `{entity}`"
                )));
            }
            if workflow.runner != AppManifestRunner::Recipe
                && !workflow.result.entities.contains(entity)
            {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` may mutate `{entity}` without returning its typed \
                     projection"
                )));
            }
        }
    }

    for (action_name, action) in &manifest.app.actions {
        if !manifest.app.workflows.contains_key(&action.workflow) {
            return Err(AppManifestError::InvalidManifest(format!(
                "action `{action_name}` references missing workflow `{}`",
                action.workflow
            )));
        }
        let expected_input = format!("{}.input", action.workflow);
        let expected_result = format!("{}.result", action.workflow);
        if action.input_from.as_str() != expected_input
            || action.result_from.as_str() != expected_result
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "action `{action_name}` must bind the selected workflow's exact input and result"
            )));
        }
    }
    for (workflow_id, workflow) in &manifest.app.workflows {
        if !workflow.contribution_ports.is_empty()
            && !manifest
                .app
                .actions
                .values()
                .any(|action| action.workflow == *workflow_id)
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_id}` declares contribution ports without an exact action \
                 binding"
            )));
        }
        if !workflow.notification_ports.is_empty()
            && !manifest
                .app
                .actions
                .values()
                .any(|action| action.workflow == *workflow_id)
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_id}` declares notification ports without an exact action binding"
            )));
        }
    }

    validate_behavior_declarations(manifest, limits)?;
    validate_event_behavior_declarations(manifest, limits)?;
    validate_notification_declarations(manifest)?;
    validate_unique_paths("assets", &manifest.app.assets, limits.max_assets)?;
    validate_resources(&manifest.app.resources)?;
    manifest.dependency_requirements(limits)?;
    Ok(())
}

/// Validate the additive `app.memory` request block (`app_memory_read_v1`).
/// The block is inert review material: it must be feature-gated, name only
/// app-readable tiers and valid agent ids, and state a purpose. Authority comes
/// only from the owner's grant, validated separately against this request.
fn validate_memory_access(manifest: &AppPackageManifest) -> Result<(), AppManifestError> {
    let Some(memory) = &manifest.app.memory else {
        return Ok(());
    };
    if !manifest
        .metadata
        .magician
        .required_features
        .contains(&AppManifestFeature::AppMemoryReadV1)
    {
        return Err(AppManifestError::InvalidManifest(
            "app declares memory without required feature app_memory_read_v1".to_owned(),
        ));
    }
    let Some(read) = &memory.read else {
        return Err(AppManifestError::InvalidManifest(
            "app.memory must declare a read request".to_owned(),
        ));
    };
    super::memory_access::validate_memory_read_request(read)
        .map_err(AppManifestError::InvalidManifest)
}

/// Validate the additive `app.llm_operations` declaration block (plan 1.4).
///
/// This is the manifest-validation-time half of the operation lane's trust
/// boundary: the block is inert review material here, and it is rejected
/// unless the package explicitly requires `llm_operations_v1` — the same
/// feature-gate discipline `contribution_ports` established. Admission
/// against the live operator trust policy happens separately in
/// `super::llm_operations`, which every execution-bound caller must pass.
fn validate_llm_operations(manifest: &AppPackageManifest) -> Result<(), AppManifestError> {
    let operations = &manifest.app.llm_operations;
    if operations.is_empty() {
        return Ok(());
    }
    if !manifest
        .metadata
        .magician
        .required_features
        .contains(&AppManifestFeature::LlmOperationsV1)
    {
        return Err(AppManifestError::InvalidManifest(
            "app declares llm_operations without required feature llm_operations_v1".to_owned(),
        ));
    }
    validate_named_map(
        "llm_operations",
        operations,
        AppContractLimits::default().max_collection_items(),
    )?;
    for (name, declaration) in operations {
        if declaration.purpose.trim().is_empty()
            || declaration.purpose.len() > APP_LLM_OPERATION_MAX_PURPOSE_BYTES
            || declaration
                .purpose
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "llm operation `{name}` purpose must contain 1..={} non-control bytes",
                APP_LLM_OPERATION_MAX_PURPOSE_BYTES
            )));
        }
        if declaration
            .max_tokens
            .is_some_and(|tokens| tokens == 0 || tokens > u64::from(u32::MAX))
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "llm operation `{name}` max_tokens hint must fit the positive execution-domain range"
            )));
        }
    }
    Ok(())
}

/// Digest of an ordered operation recipe, or `None` when there is none.
///
/// `None` for an empty recipe on purpose: a behavior that declares no steps
/// keeps a byte-identical review digest to the one it had before this contract
/// existed, so adding the field does not invalidate every installed grant.
pub fn app_behavior_steps_digest(
    steps: &[AppManifestBehaviorStep],
) -> Result<Option<AppDigest>, AppManifestError> {
    if steps.is_empty() {
        return Ok(None);
    }
    let value = serde_json::to_value(steps)
        .map_err(|error| AppManifestError::CanonicalEncoding(error.to_string()))?;
    let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "protocol": "magician.app-behavior-recipe.v1",
        "steps": value,
    }))
    .map_err(|error| AppManifestError::CanonicalEncoding(error.to_string()))?;
    Ok(Some(digest))
}

/// Validate one behavior's ordered operation recipe.
///
/// The rules exist so the recipe can never be *inferred*. An allow-set that
/// cannot be executed without guessing is why the LLM dispatcher was left
/// unreachable; these checks are what make executing one safe.
fn validate_behavior_steps(
    behavior: &AppManifestBehavior,
    manifest: &AppPackageManifest,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    if behavior.steps.is_empty() {
        // A behavior that may use LLM operations must say in what order. The
        // legacy single-schema form remains valid only for a behavior that
        // uses exactly one operation, where "the order" is not a question.
        if behavior.operations.len() > 1 {
            return Err(AppManifestError::InvalidManifest(format!(
                "behavior `{}` declares {} LLM operations and no `steps`; an unordered \
                 allow-set is not a recipe",
                behavior.id,
                behavior.operations.len()
            )));
        }
        return Ok(());
    }
    if behavior.operations.is_empty() {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{}` declares steps without any reviewed LLM operation",
            behavior.id
        )));
    }
    // The two schema forms are mutually exclusive: steps own their own output,
    // and a behavior-level schema alongside them would be a second source of
    // truth that can drift from the step that actually produces the bytes.
    if behavior.output_schema.is_some() {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{}` cannot declare both `steps` and a behavior-level `output_schema`",
            behavior.id
        )));
    }
    validate_vec_limit(
        "behavior.steps",
        behavior.steps.len(),
        APP_BEHAVIOR_MAX_STEPS,
    )?;

    let allowed = behavior.operations.iter().collect::<BTreeSet<_>>();
    let mut seen: BTreeMap<&AppName, &AppManifestBehaviorStep> = BTreeMap::new();
    for step in &behavior.steps {
        if seen.contains_key(&step.id) {
            return Err(AppManifestError::InvalidManifest(format!(
                "behavior `{}` step id `{}` is declared more than once",
                behavior.id, step.id
            )));
        }
        if !allowed.contains(&step.operation) {
            return Err(AppManifestError::InvalidManifest(format!(
                "behavior `{}` step `{}` names operation `{}`, which is not in its reviewed \
                 allow-set",
                behavior.id, step.id, step.operation
            )));
        }
        validate_behavior_output_schema(&behavior.id, &step.output_schema, manifest, limits)?;

        if let Some(guard) = step.when.as_ref() {
            // Only an EARLIER step: `seen` holds exactly the steps already
            // validated, so a self or forward reference cannot resolve and a
            // recipe cannot describe a cycle.
            let Some(source) = seen.get(&guard.step) else {
                return Err(AppManifestError::InvalidManifest(format!(
                    "behavior `{}` step `{}` guards on `{}`, which is not an earlier step",
                    behavior.id, step.id, guard.step
                )));
            };
            let Some(field) = source.output_schema.fields.get(&guard.field) else {
                return Err(AppManifestError::InvalidManifest(format!(
                    "behavior `{}` step `{}` guards on field `{}`, which step `{}` does not \
                     produce",
                    behavior.id, step.id, guard.field, guard.step
                )));
            };
            validate_behavior_step_guard_value(behavior, step, guard, field)?;
        }
        seen.insert(&step.id, step);
    }

    // Every allowed operation must be reachable. An operation granted but
    // never stepped is authority nothing can use, which reads to a reviewer as
    // capability the app has.
    for operation in &behavior.operations {
        if !behavior
            .steps
            .iter()
            .any(|step| &step.operation == operation)
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "behavior `{}` allows operation `{operation}` that no step uses",
                behavior.id
            )));
        }
    }
    Ok(())
}

/// A guard's `equals` must be a value the guarded field can actually take.
///
/// Without this an enum typo produces a step that silently never runs, which
/// looks identical to a working recipe until someone notices the app is quiet.
fn validate_behavior_step_guard_value(
    behavior: &AppManifestBehavior,
    step: &AppManifestBehaviorStep,
    guard: &AppManifestBehaviorStepGuard,
    field: &AppManifestField,
) -> Result<(), AppManifestError> {
    if guard.equals.trim().is_empty()
        || guard.equals.len() > APP_BEHAVIOR_MAX_GUARD_VALUE_BYTES
        || guard.equals.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{}` step `{}` guard value must be 1..={} non-control bytes",
            behavior.id, step.id, APP_BEHAVIOR_MAX_GUARD_VALUE_BYTES
        )));
    }
    match field {
        AppManifestField::Enum { values, .. } => {
            if !values.iter().any(|value| value.as_str() == guard.equals) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "behavior `{}` step `{}` guard value `{}` is not one of field `{}`'s \
                     declared values",
                    behavior.id, step.id, guard.equals, guard.field
                )));
            }
            Ok(())
        },
        AppManifestField::Boolean { .. } => {
            if guard.equals == "true" || guard.equals == "false" {
                Ok(())
            } else {
                Err(AppManifestError::InvalidManifest(format!(
                    "behavior `{}` step `{}` guards a boolean field on `{}`",
                    behavior.id, step.id, guard.equals
                )))
            }
        },
        AppManifestField::Text { .. } => Ok(()),
        // Numbers, timestamps, markdown and references are deliberately not
        // guardable. Equality on a formatted number or an instant invites a
        // recipe that works until a representation changes under it.
        _ => Err(AppManifestError::InvalidManifest(format!(
            "behavior `{}` step `{}` cannot guard on field `{}`; guards compare enum, text \
             or boolean",
            behavior.id, step.id, guard.field
        ))),
    }
}

/// Validate the closed, additive `app_behaviors_v1` review vocabulary.
///
/// The declaration and its resource entry are a pair: neither side may exist
/// alone, and the exact behavior/action/operation/schema/resource material is
/// subsequently folded into requested authority. Runtime scheduling remains a
/// separate, live-authority consumer.
fn validate_behavior_declarations(
    manifest: &AppPackageManifest,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    let has_feature = manifest
        .metadata
        .magician
        .required_features
        .contains(&AppManifestFeature::AppBehaviorsV1);
    let has_declarations = !manifest.app.behaviors.is_empty();
    let has_resources = !manifest.app.resources.behaviors.is_empty();
    if (has_declarations || has_resources) && !has_feature {
        return Err(AppManifestError::InvalidManifest(
            "app declares behaviors or behavior resources without required feature app_behaviors_v1"
                .to_owned(),
        ));
    }
    if has_feature && !has_declarations {
        return Err(AppManifestError::InvalidManifest(
            "app requires app_behaviors_v1 without any behavior declaration".to_owned(),
        ));
    }
    if !has_declarations {
        return Ok(());
    }
    validate_vec_limit(
        "behaviors",
        manifest.app.behaviors.len(),
        APP_BEHAVIOR_MAX_DECLARATIONS,
    )?;
    if manifest.app.resources.behaviors.len() != manifest.app.behaviors.len() {
        return Err(AppManifestError::InvalidManifest(
            "app.resources.behaviors must contain exactly one entry for every declared behavior"
                .to_owned(),
        ));
    }

    let mut ids = BTreeSet::new();
    let mut actions = BTreeSet::new();
    for behavior in &manifest.app.behaviors {
        if !ids.insert(behavior.id.clone()) {
            return Err(AppManifestError::InvalidManifest(format!(
                "behavior id `{}` is declared more than once",
                behavior.id
            )));
        }
        if !actions.insert(behavior.action.clone()) {
            return Err(AppManifestError::InvalidManifest(format!(
                "scheduled action `{}` is bound by more than one behavior",
                behavior.action
            )));
        }
        if behavior.purpose.trim().is_empty()
            || behavior.purpose.len() > APP_BEHAVIOR_MAX_PURPOSE_BYTES
            || behavior.purpose.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "behavior `{}` purpose must contain 1..={} non-control bytes",
                behavior.id, APP_BEHAVIOR_MAX_PURPOSE_BYTES
            )));
        }
        let min_interval_seconds = behavior.cadence.min_interval_seconds();
        if !(APP_BEHAVIOR_MIN_INTERVAL_SECONDS..=APP_BEHAVIOR_MAX_INTERVAL_SECONDS)
            .contains(&min_interval_seconds)
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "behavior `{}` cadence is outside app_behaviors_v1 bounds",
                behavior.id
            )));
        }
        let action = manifest.app.actions.get(&behavior.action).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "behavior `{}` names missing action `{}`",
                behavior.id, behavior.action
            ))
        })?;
        let workflow = manifest
            .app
            .workflows
            .get(&action.workflow)
            .ok_or_else(|| {
                AppManifestError::InvalidManifest(format!(
                    "behavior `{}` action names missing workflow `{}`",
                    behavior.id, action.workflow
                ))
            })?;
        if workflow.trigger != AppManifestTrigger::Schedule {
            return Err(AppManifestError::InvalidManifest(format!(
                "behavior `{}` action must bind a schedule-triggered workflow",
                behavior.id
            )));
        }
        // The manifest can require explicit model steps; only dependency locking
        // can inspect the immutable recipe root and permit its exact contextual
        // round step. Mechanical roots reject all model steps at that boundary.
        match workflow.runner {
            AppManifestRunner::Recipe
                if !behavior.operations.is_empty() && behavior.steps.is_empty() =>
            {
                return Err(AppManifestError::InvalidManifest(format!(
                    "behavior `{}` recipe model operations require explicit reviewed semantic steps",
                    behavior.id
                )));
            },
            AppManifestRunner::Auto if behavior.operations.is_empty() => {
                return Err(AppManifestError::InvalidManifest(format!(
                    "behavior `{}` auto runner requires at least one reviewed LLM operation",
                    behavior.id
                )));
            },
            AppManifestRunner::Auto | AppManifestRunner::Recipe => {},
        }
        validate_behavior_input_selector(behavior, workflow, manifest, limits)?;

        validate_vec_limit(
            "behavior.operations",
            behavior.operations.len(),
            APP_BEHAVIOR_MAX_OPERATIONS,
        )?;
        if behavior.operations.iter().collect::<BTreeSet<_>>().len() != behavior.operations.len() {
            return Err(AppManifestError::DuplicateNormalizedValue {
                field: "behavior.operations",
            });
        }
        for operation in &behavior.operations {
            if !manifest.app.llm_operations.contains_key(operation) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "behavior `{}` names undeclared app LLM operation `{operation}`",
                    behavior.id
                )));
            }
        }
        if behavior.operations.is_empty() && behavior.output_schema.is_some() {
            return Err(AppManifestError::InvalidManifest(format!(
                "behavior `{}` cannot declare model output_schema without an LLM operation",
                behavior.id
            )));
        }
        if let Some(schema) = behavior.output_schema.as_ref() {
            validate_behavior_output_schema(&behavior.id, schema, manifest, limits)?;
        }
        validate_behavior_steps(behavior, manifest, limits)?;

        let resources = manifest
            .app
            .resources
            .behaviors
            .get(&behavior.id)
            .ok_or_else(|| {
                AppManifestError::InvalidManifest(format!(
                    "behavior `{}` is missing its exact resource ceiling",
                    behavior.id
                ))
            })?;
        validate_behavior_resources(
            &behavior.id,
            min_interval_seconds,
            resources,
            &manifest.app.resources,
        )?;
    }
    if manifest
        .app
        .resources
        .behaviors
        .keys()
        .any(|id| !ids.contains(id))
    {
        return Err(AppManifestError::InvalidManifest(
            "app.resources.behaviors contains an undeclared behavior id".to_owned(),
        ));
    }
    for (workflow_id, workflow) in &manifest.app.workflows {
        if workflow.runner != AppManifestRunner::Recipe
            || workflow.trigger != AppManifestTrigger::Schedule
        {
            continue;
        }
        let mut workflow_has_action = false;
        for (action_id, action) in &manifest.app.actions {
            if action.workflow != *workflow_id {
                continue;
            }
            workflow_has_action = true;
            if !actions.contains(action_id) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "scheduled recipe action `{action_id}` must be bound by exactly one declared behavior"
                )));
            }
        }
        if !workflow_has_action {
            return Err(AppManifestError::InvalidManifest(format!(
                "scheduled recipe workflow `{workflow_id}` must have an action bound by exactly one declared behavior"
            )));
        }
    }
    Ok(())
}

fn validate_event_behavior_declarations(
    manifest: &AppPackageManifest,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    let has_feature = manifest
        .metadata
        .magician
        .required_features
        .contains(&AppManifestFeature::AppEventBehaviorsV1);
    let has_declarations = !manifest.app.event_behaviors.is_empty();
    let has_resources = !manifest.app.resources.event_behaviors.is_empty();
    if (has_declarations || has_resources) && !has_feature {
        return Err(AppManifestError::InvalidManifest(
            "app declares event_behaviors or their resources without required feature app_event_behaviors_v1"
                .to_owned(),
        ));
    }
    if has_feature && !has_declarations {
        return Err(AppManifestError::InvalidManifest(
            "app requires app_event_behaviors_v1 without an event behavior declaration".to_owned(),
        ));
    }
    if !has_declarations {
        return Ok(());
    }
    validate_vec_limit(
        "event_behaviors",
        manifest.app.event_behaviors.len(),
        APP_EVENT_BEHAVIOR_MAX_DECLARATIONS,
    )?;
    if manifest.app.resources.event_behaviors.len() != manifest.app.event_behaviors.len() {
        return Err(AppManifestError::InvalidManifest(
            "app.resources.event_behaviors must contain exactly one entry for every event behavior"
                .to_owned(),
        ));
    }

    let mut ids = BTreeSet::new();
    let mut actions = BTreeSet::new();
    for behavior in &manifest.app.event_behaviors {
        if !ids.insert(behavior.id.clone()) {
            return Err(AppManifestError::InvalidManifest(format!(
                "event behavior id `{}` is declared more than once",
                behavior.id
            )));
        }
        if !actions.insert(behavior.action.clone()) {
            return Err(AppManifestError::InvalidManifest(format!(
                "event action `{}` is bound by more than one event behavior",
                behavior.action
            )));
        }
        if behavior.purpose.trim().is_empty()
            || behavior.purpose.len() > APP_BEHAVIOR_MAX_PURPOSE_BYTES
            || behavior.purpose.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "event behavior `{}` purpose must contain 1..={} non-control bytes",
                behavior.id, APP_BEHAVIOR_MAX_PURPOSE_BYTES
            )));
        }
        if !(APP_BEHAVIOR_MIN_INTERVAL_SECONDS..=APP_BEHAVIOR_MAX_INTERVAL_SECONDS)
            .contains(&behavior.min_interval_seconds)
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "event behavior `{}` interval is outside app_event_behaviors_v1 bounds",
                behavior.id
            )));
        }
        let AppEventSubscriptionV1::InstallationExecutionTerminal { outcomes } =
            &behavior.subscription;
        if outcomes.is_empty()
            || outcomes.len() > 3
            || outcomes.iter().collect::<BTreeSet<_>>().len() != outcomes.len()
            || outcomes.windows(2).any(|pair| pair[0] >= pair[1])
            // Reserved in the closed enum for a later canonical producer, but
            // the current post-persistence rail proves only completed success
            // and failure. Admission must not promise cancellation yet.
            || outcomes.contains(&AppEventTerminalOutcomeV1::Cancelled)
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "event behavior `{}` terminal outcomes must be nonempty, unique, and canonical",
                behavior.id
            )));
        }
        let action = manifest.app.actions.get(&behavior.action).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "event behavior `{}` names missing action `{}`",
                behavior.id, behavior.action
            ))
        })?;
        let workflow = manifest
            .app
            .workflows
            .get(&action.workflow)
            .ok_or_else(|| {
                AppManifestError::InvalidManifest(format!(
                    "event behavior `{}` action names missing workflow `{}`",
                    behavior.id, action.workflow
                ))
            })?;
        if workflow.trigger != AppManifestTrigger::Event {
            return Err(AppManifestError::InvalidManifest(format!(
                "event behavior `{}` action must bind an event-triggered workflow",
                behavior.id
            )));
        }
        // The manifest can require explicit model steps; only dependency locking
        // can inspect the immutable recipe root and permit its exact contextual
        // round step. Mechanical roots reject all model steps at that boundary.
        match workflow.runner {
            AppManifestRunner::Recipe
                if !behavior.operations.is_empty() && behavior.steps.is_empty() =>
            {
                return Err(AppManifestError::InvalidManifest(format!(
                    "event behavior `{}` recipe model operations require explicit reviewed semantic steps",
                    behavior.id
                )));
            },
            AppManifestRunner::Auto if behavior.operations.is_empty() => {
                return Err(AppManifestError::InvalidManifest(format!(
                    "event behavior `{}` auto runner requires at least one reviewed LLM operation",
                    behavior.id
                )));
            },
            AppManifestRunner::Auto | AppManifestRunner::Recipe => {},
        }
        let expected_schema = event_behavior_projection_schema(&behavior.subscription);
        if workflow.input.compiled_value_schema()?.source()
            != expected_schema.compiled_value_schema()?.source()
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "event behavior `{}` workflow input must exactly match its host event projection",
                behavior.id
            )));
        }
        validate_vec_limit(
            "event_behavior.operations",
            behavior.operations.len(),
            APP_BEHAVIOR_MAX_OPERATIONS,
        )?;
        if behavior.operations.iter().collect::<BTreeSet<_>>().len() != behavior.operations.len() {
            return Err(AppManifestError::DuplicateNormalizedValue {
                field: "event_behavior.operations",
            });
        }
        for operation in &behavior.operations {
            if !manifest.app.llm_operations.contains_key(operation) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "event behavior `{}` names undeclared app LLM operation `{operation}`",
                    behavior.id
                )));
            }
        }
        if behavior.operations.is_empty() && behavior.output_schema.is_some() {
            return Err(AppManifestError::InvalidManifest(format!(
                "event behavior `{}` cannot declare model output_schema without an LLM operation",
                behavior.id
            )));
        }
        if let Some(schema) = behavior.output_schema.as_ref() {
            validate_behavior_output_schema(&behavior.id, schema, manifest, limits)?;
        }
        let resources = manifest
            .app
            .resources
            .event_behaviors
            .get(&behavior.id)
            .ok_or_else(|| {
                AppManifestError::InvalidManifest(format!(
                    "event behavior `{}` is missing its exact resource ceiling",
                    behavior.id
                ))
            })?;
        validate_behavior_resources(
            &behavior.id,
            behavior.min_interval_seconds,
            resources,
            &manifest.app.resources,
        )?;
    }
    if manifest
        .app
        .resources
        .event_behaviors
        .keys()
        .any(|id| !ids.contains(id))
    {
        return Err(AppManifestError::InvalidManifest(
            "app.resources.event_behaviors contains an undeclared event behavior id".to_owned(),
        ));
    }
    for (workflow_id, workflow) in &manifest.app.workflows {
        if workflow.runner != AppManifestRunner::Recipe
            || workflow.trigger != AppManifestTrigger::Event
        {
            continue;
        }
        let mut workflow_has_action = false;
        for (action_id, action) in &manifest.app.actions {
            if action.workflow != *workflow_id {
                continue;
            }
            workflow_has_action = true;
            if !actions.contains(action_id) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "event recipe action `{action_id}` must be bound by exactly one event behavior"
                )));
            }
        }
        if !workflow_has_action {
            return Err(AppManifestError::InvalidManifest(format!(
                "event recipe workflow `{workflow_id}` must have an action bound by exactly one event behavior"
            )));
        }
    }
    Ok(())
}

fn validate_workflow_notification_ports(
    workflow_id: &AppName,
    workflow: &AppManifestWorkflow,
    feature_enabled: bool,
) -> Result<(), AppManifestError> {
    if workflow.notification_ports.is_empty() {
        return Ok(());
    }
    if !feature_enabled {
        return Err(AppManifestError::InvalidManifest(format!(
            "workflow `{workflow_id}` declares notification_ports without required feature app_owner_notifications_v1"
        )));
    }
    validate_named_map(
        "workflow.notification_ports",
        &workflow.notification_ports,
        APP_NOTIFICATION_MAX_PORTS_PER_WORKFLOW,
    )?;
    for (port_id, port) in &workflow.notification_ports {
        if port.purpose.trim().is_empty()
            || port.purpose.len() > APP_NOTIFICATION_MAX_PURPOSE_BYTES
            || port.purpose.bytes().any(|byte| byte.is_ascii_control())
            || port.max_notifications_per_period == 0
            || port.max_notifications_per_period > APP_NOTIFICATION_MAX_PER_PERIOD
            || u64::from(port.max_notifications_per_period)
                > port.period_seconds.div_ceil(APP_NOTIFICATION_MIN_PERIOD_SECONDS)
            || !(APP_NOTIFICATION_MIN_PERIOD_SECONDS..=APP_NOTIFICATION_MAX_PERIOD_SECONDS)
                .contains(&port.period_seconds)
            || port.max_pending == 0
            || port.max_pending > APP_NOTIFICATION_MAX_PENDING
            || !(APP_NOTIFICATION_MIN_TTL_SECONDS..=APP_NOTIFICATION_MAX_TTL_SECONDS)
                .contains(&port.ttl_seconds)
            // The durable notification registry currently admits info and
            // warning only. Keep Critical reserved in the vocabulary without
            // granting a value its effect owner cannot persist.
            || port.severity_ceiling == AppNotificationSeverityV1::Critical
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_id}` notification port `{port_id}` is outside app_owner_notifications_v1 bounds"
            )));
        }
    }
    Ok(())
}

fn validate_notification_declarations(
    manifest: &AppPackageManifest,
) -> Result<(), AppManifestError> {
    let has_feature = manifest
        .metadata
        .magician
        .required_features
        .contains(&AppManifestFeature::AppOwnerNotificationsV1);
    let total_ports = manifest
        .app
        .workflows
        .values()
        .map(|workflow| workflow.notification_ports.len())
        .sum::<usize>();
    if has_feature && total_ports == 0 {
        return Err(AppManifestError::InvalidManifest(
            "app requires app_owner_notifications_v1 without a notification port".to_owned(),
        ));
    }
    if total_ports > APP_NOTIFICATION_MAX_TOTAL_PORTS {
        return Err(AppManifestError::CollectionLimit {
            field: "workflow.notification_ports",
            limit: APP_NOTIFICATION_MAX_TOTAL_PORTS,
        });
    }
    Ok(())
}

fn validate_behavior_input_selector(
    behavior: &AppManifestBehavior,
    workflow: &AppManifestWorkflow,
    manifest: &AppPackageManifest,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    validate_nonempty_unique_values(
        "behavior.input.fields",
        &behavior.input.fields,
        APP_BEHAVIOR_MAX_INPUT_FIELDS,
    )?;
    let entity = manifest
        .app
        .entities
        .get(&behavior.input.entity)
        .ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "behavior `{}` input names missing entity `{}`",
                behavior.id, behavior.input.entity
            ))
        })?;
    let mut projected_fields = BTreeMap::new();
    for field_name in &behavior.input.fields {
        let field = entity.fields.get(field_name).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "behavior `{}` input selects missing field `{field_name}` from entity `{}`",
                behavior.id, behavior.input.entity
            ))
        })?;
        validate_field(field, &manifest.app.entities, limits)?;
        projected_fields.insert(field_name.clone(), field.clone());
    }
    let projected_schema = AppManifestInputSchema {
        schema_type: AppManifestSchemaType::Object,
        fields: projected_fields,
        value_schema: None,
    };
    let projected_compiled = projected_schema.compiled_value_schema()?;
    let workflow_compiled = workflow.input.compiled_value_schema()?;
    let projected_source = projected_compiled.source();
    let workflow_source = workflow_compiled.source();
    // The selected record projection must be structurally exact after arena
    // normalization. Handling floors are deliberately excluded: workflow
    // admission joins its declared floor with the envelope's effective field
    // policy, so a safer workflow floor remains valid and a weaker floor can
    // never lower the selected record's classification/model restriction.
    if projected_source.version != workflow_source.version
        || projected_source.root != workflow_source.root
        || projected_source.nodes != workflow_source.nodes
    {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{}` scheduled workflow input must exactly match its typed singleton projection",
            behavior.id
        )));
    }
    Ok(())
}

fn validate_behavior_output_schema(
    behavior_id: &AppName,
    schema: &AppManifestInputSchema,
    manifest: &AppPackageManifest,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    if schema.value_schema.is_some() && !schema.fields.is_empty() {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{behavior_id}` output_schema must use either legacy fields or one canonical value_schema"
        )));
    }
    if schema.fields.is_empty() && schema.value_schema.is_none() {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{behavior_id}` output_schema must not be empty"
        )));
    }
    let field_limit = limits
        .max_fields
        .min(AppContractLimits::default().max_collection_items());
    validate_named_map("behavior.output_schema.fields", &schema.fields, field_limit)?;
    for field in schema.fields.values() {
        validate_field(field, &manifest.app.entities, limits)?;
    }
    if !matches!(
        schema.compiled_value_schema()?.root_node(),
        AppWorkflowValueTypeNode::Record { .. }
    ) {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{behavior_id}` output_schema root must be one closed record"
        )));
    }
    Ok(())
}

fn validate_behavior_resources(
    behavior_id: &AppName,
    min_interval_seconds: u64,
    resources: &AppManifestBehaviorResources,
    app_resources: &AppManifestResources,
) -> Result<(), AppManifestError> {
    if resources.per_run.max_tokens == 0
        || resources.per_run.max_cost_usd.microusd() == 0
        || resources.per_run.max_active_seconds == 0
        || resources.monthly.max_tokens == 0
        || resources.monthly.max_cost_usd.microusd() == 0
        || resources.max_starts_per_period == 0
        || resources.max_starts_per_period > APP_BEHAVIOR_MAX_STARTS_PER_PERIOD
        || resources.period_seconds < min_interval_seconds
        || resources.period_seconds > APP_BEHAVIOR_MAX_PERIOD_SECONDS
        || resources.max_causation_depth == 0
        || resources.max_causation_depth > APP_BEHAVIOR_MAX_CAUSATION_DEPTH
        || resources.max_spend_depth == 0
        || resources.max_spend_depth > APP_BEHAVIOR_MAX_SPEND_DEPTH
        || resources.max_contribution_proposals_per_run
            > APP_BEHAVIOR_MAX_CONTRIBUTION_PROPOSALS_PER_RUN
    {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{behavior_id}` resource ceiling is outside app_behaviors_v1 bounds"
        )));
    }
    let cadence_start_ceiling = resources.period_seconds.div_ceil(min_interval_seconds);
    if u64::from(resources.max_starts_per_period) > cadence_start_ceiling {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{behavior_id}` start cap exceeds what its cadence can produce"
        )));
    }
    if resources.per_run.max_tokens > resources.monthly.max_tokens
        || resources.per_run.max_cost_usd > resources.monthly.max_cost_usd
        || resources.per_run.max_tokens > app_resources.per_run.max_tokens
        || resources.per_run.max_cost_usd > app_resources.per_run.max_cost_usd
        || resources.per_run.max_active_seconds > app_resources.per_run.max_active_seconds
        || resources.monthly.max_tokens > app_resources.monthly.max_tokens
        || resources.monthly.max_cost_usd > app_resources.monthly.max_cost_usd
    {
        return Err(AppManifestError::InvalidManifest(format!(
            "behavior `{behavior_id}` resource ceiling widens the package resource ceiling"
        )));
    }
    Ok(())
}

fn validate_declared_dependencies(
    manifest: &AppPackageManifest,
    limits: &AppPackageLimits,
) -> Result<BTreeSet<String>, AppManifestError> {
    let declared_tools = manifest.declared_tools()?;
    if manifest.app.dependencies.procedure_skills.len() + declared_tools.len()
        > limits.max_dependencies
    {
        return Err(AppManifestError::DependencyLimit {
            limit: limits.max_dependencies,
        });
    }
    let mut skills = BTreeSet::new();
    let mut vendored_paths = BTreeSet::new();
    for dependency in &manifest.app.dependencies.procedure_skills {
        if !skills.insert(normalized_collision_key(dependency.skill.as_str())) {
            return Err(AppManifestError::DuplicateNormalizedIdentifier(
                dependency.skill.to_string(),
            ));
        }
        validate_version_requirement(&dependency.version_requirement, limits)?;
        if let Some(path) = &dependency.vendored_path {
            if !path.as_str().starts_with("vendor/skills/") || !path.as_str().ends_with("/SKILL.md")
            {
                return Err(AppManifestError::InvalidManifest(format!(
                    "vendored procedure `{}` must identify vendor/skills/<name>/SKILL.md",
                    dependency.skill
                )));
            }
            if !vendored_paths.insert(path.collision_key()) {
                return Err(AppManifestError::DuplicateNormalizedValue {
                    field: "dependencies.procedure_skills.vendored_path",
                });
            }
        }
    }
    let used = manifest
        .app
        .workflows
        .values()
        .flat_map(|workflow| &workflow.uses)
        .map(|name| (normalized_collision_key(name.as_str()), name))
        .collect::<BTreeMap<_, _>>();
    let mut tools = BTreeSet::new();
    for dependency in &declared_tools {
        let key = normalized_collision_key(dependency.name.as_str());
        if !tools.insert(key.clone()) {
            return Err(AppManifestError::DuplicateNormalizedIdentifier(
                dependency.name.to_string(),
            ));
        }
        if !used.contains_key(&key) {
            return Err(AppManifestError::InvalidManifest(format!(
                "declared tool `{}` is not used by a workflow",
                dependency.name
            )));
        }
        if dependency
            .primitive_ref
            .as_ref()
            .is_some_and(|reference| !reference.as_str().starts_with("primitive:tool-skill:"))
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "tool `{}` primitive_ref must be a qualified tool-skill primitive identity",
                dependency.name
            )));
        }
        if dependency.actions.len() > limits.max_actions {
            return Err(AppManifestError::InvalidManifest(format!(
                "tool `{}` action selector count exceeds {}",
                dependency.name, limits.max_actions
            )));
        }
        let mut selected_actions = BTreeSet::new();
        for selector in &dependency.actions {
            let selector = selector.trim();
            if selector.is_empty()
                || selector.len() > limits.max_string_bytes
                || !selected_actions.insert(normalized_collision_key(selector))
            {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tool `{}` action selectors must be non-empty, bounded and unique",
                    dependency.name
                )));
            }
        }
        if let Some(interactive) = &dependency.interactive {
            interactive.validate_for_admission().map_err(|error| {
                AppManifestError::InvalidManifest(format!(
                    "tool `{}` interactive capability request is invalid: {error}",
                    dependency.name
                ))
            })?;
            if dependency.actions.len() != 1 {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tool `{}` interactive capability request requires one exact selected action",
                    dependency.name
                )));
            }
        }
        validate_version_requirement(&dependency.version_requirement, limits)?;
    }
    if let Some((_, tool)) = used.iter().find(|(key, _)| !tools.contains(key.as_str())) {
        return Err(AppManifestError::InvalidManifest(format!(
            "workflow uses undeclared tool `{tool}`"
        )));
    }
    Ok(skills)
}

fn validate_field(
    field: &AppManifestField,
    entities: &BTreeMap<AppName, AppManifestEntity>,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    if let Some(policy) = field.policy() {
        validate_policy_override(policy, limits)?;
    }
    match field {
        AppManifestField::Enum { values, .. } => {
            if values.is_empty() {
                return Err(AppManifestError::InvalidManifest(
                    "enum fields require at least one value".to_owned(),
                ));
            }
            validate_vec_limit("enum.values", values.len(), limits.max_fields)?;
            validate_unique_names("enum.values", values)?;
        },
        AppManifestField::Reference {
            entity,
            nullable,
            on_delete,
            cycle_policy,
            max_traversal_depth,
            ..
        } => {
            if !entities.contains_key(entity) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "reference field targets missing entity `{entity}`"
                )));
            }
            if *max_traversal_depth == 0 || usize::from(*max_traversal_depth) > 16 {
                return Err(AppManifestError::InvalidManifest(
                    "reference max_traversal_depth must be between 1 and 16".to_owned(),
                ));
            }
            if *on_delete == AppReferenceDeletePolicy::Nullify && !*nullable {
                return Err(AppManifestError::InvalidManifest(
                    "reference on_delete nullify requires a nullable field".to_owned(),
                ));
            }
            if *on_delete == AppReferenceDeletePolicy::Cascade
                && *cycle_policy == AppReferenceCyclePolicy::AllowBounded
            {
                return Err(AppManifestError::InvalidManifest(
                    "cascade delete cannot be combined with an allowed reference cycle".to_owned(),
                ));
            }
        },
        _ => {},
    }
    Ok(())
}

fn validate_view(
    name: &AppName,
    view: &AppManifestView,
    entity: &AppManifestEntity,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    if view.kind == AppManifestViewKind::Board {
        return Err(AppManifestError::UnsupportedViewKind {
            view: name.to_string(),
            kind: "board",
        });
    }
    validate_declarative_surface(name, view, entity, limits)?;
    for parameter in view.route.parameter_names() {
        let parameter = AppName::parse(parameter)
            .map_err(|_| AppManifestError::UnsafeRoute(view.route.as_str().to_owned()))?;
        let field = entity.fields.get(&parameter).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "view `{name}` route parameter `{parameter}` has no matching entity field"
            ))
        })?;
        if !matches!(
            field,
            AppManifestField::Text { .. }
                | AppManifestField::Integer { .. }
                | AppManifestField::Decimal { .. }
                | AppManifestField::Boolean { .. }
                | AppManifestField::Enum { .. }
                | AppManifestField::Reference { .. }
        ) {
            return Err(AppManifestError::InvalidManifest(format!(
                "view `{name}` route parameter `{parameter}` must bind to a portable scalar field"
            )));
        }
    }
    validate_vec_limit("view.columns", view.columns.len(), limits.max_fields)?;
    validate_unique_names("view.columns", &view.columns)?;
    for column in &view.columns {
        if !entity.fields.contains_key(column) {
            return Err(AppManifestError::InvalidManifest(format!(
                "view `{name}` column references missing field `{column}`"
            )));
        }
    }
    let fields = [
        view.partition_field.as_ref(),
        view.parent_field.as_ref(),
        view.order_field.as_ref(),
        view.status_field.as_ref(),
        view.timestamp_field.as_ref(),
        view.action_field.as_ref(),
        view.actor_field.as_ref(),
        view.type_field.as_ref(),
        view.target_field.as_ref(),
    ];
    for field in fields.iter().flatten() {
        if !entity.fields.contains_key(field) {
            return Err(AppManifestError::InvalidManifest(format!(
                "view `{name}` references missing field `{field}`"
            )));
        }
    }
    let tree_fields = [
        view.partition_field.as_ref(),
        view.parent_field.as_ref(),
        view.order_field.as_ref(),
        view.status_field.as_ref(),
    ];
    let timeline_fields = [
        view.timestamp_field.as_ref(),
        view.action_field.as_ref(),
        view.actor_field.as_ref(),
        view.type_field.as_ref(),
        view.target_field.as_ref(),
    ];
    match view.kind {
        AppManifestViewKind::Tree => {
            let (Some(parent), Some(order)) =
                (view.parent_field.as_ref(), view.order_field.as_ref())
            else {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tree view `{name}` requires parent_field and order_field"
                )));
            };
            let Some(partition) = view.partition_field.as_ref() else {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tree view `{name}` requires partition_field and cannot carry timeline \
                     bindings"
                )));
            };
            if timeline_fields.iter().any(|field| field.is_some()) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tree view `{name}` requires partition_field and cannot carry timeline \
                     bindings"
                )));
            }
            if !view.columns.is_empty() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tree view `{name}` cannot declare table columns"
                )));
            }
            if !matches!(
                entity.fields.get(partition),
                Some(
                    AppManifestField::Text { .. }
                        | AppManifestField::Integer { .. }
                        | AppManifestField::Enum { .. }
                        | AppManifestField::Reference { .. }
                )
            ) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tree view `{name}` partition_field must be a stable text, integer, enum or \
                     reference key"
                )));
            }
            if !matches!(
                entity.fields.get(parent),
                Some(AppManifestField::Reference {
                    entity: target,
                    nullable: true,
                    ..
                }) if target == &view.entity
            ) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tree view `{name}` parent_field must be a nullable self-reference"
                )));
            }
            if !matches!(
                entity.fields.get(order),
                Some(AppManifestField::Integer { .. } | AppManifestField::Decimal { .. })
            ) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tree view `{name}` order_field must be numeric"
                )));
            }
            if view.status_field.as_ref().is_some_and(|status| {
                !matches!(
                    entity.fields.get(status),
                    Some(AppManifestField::Enum { .. })
                )
            }) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tree view `{name}` status_field must be an enum"
                )));
            }
            if tree_display_field(view, entity).is_none() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "tree view `{name}` requires a non-structural text or markdown field for node \
                     labels"
                )));
            }
            Ok(())
        },
        AppManifestViewKind::Timeline => {
            let (Some(timestamp), Some(action)) =
                (view.timestamp_field.as_ref(), view.action_field.as_ref())
            else {
                return Err(AppManifestError::InvalidManifest(format!(
                    "timeline view `{name}` requires timestamp_field and action_field"
                )));
            };
            if tree_fields.iter().any(|field| field.is_some()) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "timeline view `{name}` cannot carry tree bindings"
                )));
            }
            if !view.columns.is_empty() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "timeline view `{name}` cannot declare table columns"
                )));
            }
            if !matches!(
                entity.fields.get(timestamp),
                Some(
                    AppManifestField::Timestamp { .. }
                        | AppManifestField::Integer { .. }
                        | AppManifestField::Decimal { .. }
                )
            ) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "timeline view `{name}` timestamp_field must be temporal or numeric"
                )));
            }
            if !matches!(
                entity.fields.get(action),
                Some(
                    AppManifestField::Text { .. }
                        | AppManifestField::Markdown { .. }
                        | AppManifestField::Enum { .. }
                )
            ) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "timeline view `{name}` action_field must be textual"
                )));
            }
            if view.actor_field.as_ref().is_some_and(|field| {
                !matches!(
                    entity.fields.get(field),
                    Some(
                        AppManifestField::Text { .. }
                            | AppManifestField::Enum { .. }
                            | AppManifestField::Reference { .. }
                    )
                )
            }) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "timeline view `{name}` actor_field must be textual, enum or reference"
                )));
            }
            if view.type_field.as_ref().is_some_and(|field| {
                !matches!(
                    entity.fields.get(field),
                    Some(AppManifestField::Text { .. } | AppManifestField::Enum { .. })
                )
            }) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "timeline view `{name}` type_field must be textual or enum"
                )));
            }
            if view.target_field.as_ref().is_some_and(|field| {
                !matches!(
                    entity.fields.get(field),
                    Some(
                        AppManifestField::Text { .. }
                            | AppManifestField::Markdown { .. }
                            | AppManifestField::Enum { .. }
                            | AppManifestField::Reference { .. }
                    )
                )
            }) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "timeline view `{name}` target_field must be textual, enum or reference"
                )));
            }
            Ok(())
        },
        AppManifestViewKind::Graph => {
            let Some(parent) = view.parent_field.as_ref() else {
                return Err(AppManifestError::InvalidManifest(format!(
                    "graph view `{name}` requires parent_field"
                )));
            };
            if timeline_fields.iter().any(|field| field.is_some()) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "graph view `{name}` cannot carry timeline bindings"
                )));
            }
            if view.partition_field.is_some() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "graph view `{name}` cannot carry tree partition bindings"
                )));
            }
            if !view.columns.is_empty() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "graph view `{name}` cannot declare table columns"
                )));
            }
            if !matches!(
                entity.fields.get(parent),
                Some(
                    AppManifestField::Reference {
                        entity: target,
                        nullable: true,
                        ..
                    }
                ) if target == &view.entity
            ) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "graph view `{name}` parent_field must be a nullable self-reference"
                )));
            }
            if view.order_field.as_ref().is_some_and(|order| {
                !matches!(
                    entity.fields.get(order),
                    Some(AppManifestField::Integer { .. } | AppManifestField::Decimal { .. })
                )
            }) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "graph view `{name}` order_field must be numeric"
                )));
            }
            if view.status_field.as_ref().is_some_and(|status| {
                !matches!(
                    entity.fields.get(status),
                    Some(AppManifestField::Enum { .. })
                )
            }) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "graph view `{name}` status_field must be an enum"
                )));
            }
            if tree_display_field(view, entity).is_none() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "graph view `{name}` requires a non-structural text or markdown field for \
                     node labels"
                )));
            }
            Ok(())
        },
        AppManifestViewKind::List | AppManifestViewKind::Table
            if fields.iter().any(|field| field.is_some()) =>
        {
            Err(AppManifestError::InvalidManifest(format!(
                "list/table view `{name}` cannot carry tree or timeline field bindings"
            )))
        },
        AppManifestViewKind::List if !view.columns.is_empty() => {
            Err(AppManifestError::InvalidManifest(format!(
                "list view `{name}` cannot declare table columns"
            )))
        },
        AppManifestViewKind::Table if view.columns.is_empty() && view.components.is_empty() => {
            Err(AppManifestError::InvalidManifest(format!(
                "table view `{name}` requires at least one declared column"
            )))
        },
        AppManifestViewKind::List | AppManifestViewKind::Table => Ok(()),
        AppManifestViewKind::Board => Err(AppManifestError::UnsupportedViewKind {
            view: name.to_string(),
            kind: "board",
        }),
    }
}

/// Select the V1 tree node label without relying on field-name conventions.
/// Text fields take precedence over Markdown and canonical BTreeMap order is
/// the stable tie-breaker. Structural tree bindings are never reused as the
/// label because their value may be an opaque partition/reference/order key.
pub fn tree_display_field<'a>(
    view: &AppManifestView,
    entity: &'a AppManifestEntity,
) -> Option<&'a AppName> {
    let is_structural = |name: &AppName| {
        [
            view.partition_field.as_ref(),
            view.parent_field.as_ref(),
            view.order_field.as_ref(),
            view.status_field.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|structural| structural == name)
    };
    entity
        .fields
        .iter()
        .find_map(|(name, field)| {
            (!is_structural(name) && matches!(field, AppManifestField::Text { .. })).then_some(name)
        })
        .or_else(|| {
            entity.fields.iter().find_map(|(name, field)| {
                (!is_structural(name) && matches!(field, AppManifestField::Markdown { .. }))
                    .then_some(name)
            })
        })
}

/// Compute the immutable digest recorded by a package revision for all
/// admitted view declarations. This lives beside manifest admission so package
/// publication and surface compilation cannot accidentally use different
/// canonicalization rules.
pub fn canonical_view_schema_digest(
    manifest: &AppPackageManifest,
) -> Result<AppDigest, AppManifestError> {
    let value = serde_json::to_value(&manifest.app.views)
        .map_err(|error| AppManifestError::CanonicalEncoding(error.to_string()))?;
    let bytes = canonical_json_bytes(&value)
        .map_err(|error| AppManifestError::CanonicalEncoding(error.to_string()))?;
    Ok(AppDigest::blake3(&bytes))
}

pub fn validate_policy(
    policy: &AppManifestPolicy,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    validate_vec_limit(
        "approved_destinations",
        policy.approved_destinations.len(),
        limits.max_dependencies,
    )?;
    validate_unique_refs("approved_destinations", &policy.approved_destinations)?;
    match policy.external_egress {
        AppExternalEgress::Denied if !policy.approved_destinations.is_empty() => {
            Err(AppManifestError::InvalidManifest(
                "approved destinations must be empty when egress is denied".to_owned(),
            ))
        },
        AppExternalEgress::ApprovedDestinations if policy.approved_destinations.is_empty() => {
            Err(AppManifestError::InvalidManifest(
                "approved destination egress requires at least one destination".to_owned(),
            ))
        },
        _ => Ok(()),
    }
}

fn validate_workflow_contribution_ports(
    workflow_name: &AppName,
    workflow: &AppManifestWorkflow,
    entities: &BTreeMap<AppName, AppManifestEntity>,
    policy: &AppManifestPolicy,
    feature_declared: bool,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    if workflow.contribution_ports.is_empty() {
        return Ok(());
    }
    if !feature_declared {
        return Err(AppManifestError::InvalidManifest(format!(
            "workflow `{workflow_name}` declares contribution ports without required feature \
             `contribution_ports_v1`"
        )));
    }
    validate_named_map(
        "workflow.contribution_ports",
        &workflow.contribution_ports,
        APP_CONTRIBUTION_MAX_PORTS_PER_WORKFLOW,
    )?;
    for (port_id, port) in &workflow.contribution_ports {
        port.frequency.validate().map_err(|error| {
            AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` contribution port `{port_id}` has invalid frequency: \
                 {error}"
            ))
        })?;
        let ttl_ms = i64::try_from(port.maximum_retention_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000));
        if port.maximum_retention_seconds == 0
            || ttl_ms.is_none()
            || ttl_ms.is_some_and(|ttl_ms| ttl_ms > APP_CONTRIBUTION_MAX_TTL_MS)
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` contribution port `{port_id}` has an unsupported \
                 maximum retention"
            )));
        }
        validate_nonempty_unique_values(
            "workflow.contribution_ports.purposes",
            &port.purposes,
            APP_CONTRIBUTION_MAX_PURPOSES.min(limits.max_dependencies),
        )?;
        if port.purposes.len() != 1 {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` contribution port `{port_id}` must select exactly one \
                 V1 purpose"
            )));
        }
        validate_nonempty_unique_values(
            "workflow.contribution_ports.audiences",
            &port.audiences,
            APP_CONTRIBUTION_MAX_AUDIENCES.min(limits.max_dependencies),
        )?;
        let fixed_destination =
            AppContributionDestinationBinding::for_destination(port.destination);
        if port.audiences.len() != 1
            || port.audiences[0].as_str() != fixed_destination.required_audience()
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` contribution port `{port_id}` audience contradicts \
                 its fixed V1 destination"
            )));
        }
        validate_nonempty_unique_values(
            "workflow.contribution_ports.evidence_classes",
            &port.evidence_classes,
            APP_CONTRIBUTION_MAX_EVIDENCE_CLASSES,
        )?;
        // The only V1 producer is the agent-authored terminal summary. Even
        // when its exact source record is authoritative, the prose derived by
        // a model is not. Keep that distinction load-bearing in the immutable
        // package declaration instead of trusting the destination to infer it.
        if port.evidence_classes.as_slice() != [AppContributionEvidenceClass::Hypothesis] {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` contribution port `{port_id}` must select the \
                 hypothesis V1 evidence class"
            )));
        }

        let AppContributionSource::MutationBackedEntityProjection {
            entity,
            selected_fields,
        } = &port.source;
        validate_nonempty_unique_values(
            "workflow.contribution_ports.source.selected_fields",
            selected_fields,
            APP_CONTRIBUTION_MAX_SELECTED_FIELDS.min(limits.max_fields),
        )?;
        if workflow.result.kind != AppManifestResultKind::EntityProjection
            || workflow.result.entities.len() != 1
            || workflow.result.entities.first() != Some(entity)
            || workflow.may_mutate.len() != 1
            || workflow.may_mutate.first() != Some(entity)
        {
            return Err(AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` contribution port `{port_id}` requires one \
                 mutation-backed entity-projection result source"
            )));
        }
        let declared = entities.get(entity).ok_or_else(|| {
            AppManifestError::InvalidManifest(format!(
                "workflow `{workflow_name}` contribution port `{port_id}` selects missing entity \
                 `{entity}`"
            ))
        })?;
        for field in selected_fields {
            let root = field.as_str().split('.').next().unwrap_or_default();
            if !declared.fields.keys().any(|name| name.as_str() == root) {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` contribution port `{port_id}` selects undeclared \
                     entity field `{field}`"
                )));
            }
        }
        match port.destination {
            AppContributionDestination::Memory
                if policy.memory_promotion != AppMemoryPromotion::CandidateAllowed =>
            {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` contribution port `{port_id}` requests memory \
                     while memory promotion is denied"
                )));
            },
            AppContributionDestination::PersonalAgentRetrieval
                if policy.personal_agent_access != AppPersonalAgentAccess::ApprovedProjection =>
            {
                return Err(AppManifestError::InvalidManifest(format!(
                    "workflow `{workflow_name}` contribution port `{port_id}` requests \
                     personal-agent retrieval while projection access is denied"
                )));
            },
            AppContributionDestination::Memory
            | AppContributionDestination::PersonalAgentRetrieval => {},
        }
    }
    Ok(())
}

fn validate_declarative_surface(
    view_name: &AppName,
    view: &AppManifestView,
    entity: &AppManifestEntity,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    if view.components.is_empty() {
        return Ok(());
    }
    if !matches!(
        view.kind,
        AppManifestViewKind::List | AppManifestViewKind::Table
    ) {
        return Err(AppManifestError::InvalidManifest(format!(
            "view `{view_name}` declarative components require list or table query semantics"
        )));
    }
    let encoded = serde_json::to_vec(&view.components)
        .map_err(|error| AppManifestError::CanonicalEncoding(error.to_string()))?;
    if encoded.len() > APP_SURFACE_MAX_DECLARATIVE_BYTES {
        return Err(AppManifestError::InvalidManifest(format!(
            "view `{view_name}` declarative component binding exceeds {} bytes",
            APP_SURFACE_MAX_DECLARATIVE_BYTES
        )));
    }

    let mut stack = view
        .components
        .iter()
        .rev()
        .map(|component| (component, 1usize))
        .collect::<Vec<_>>();
    let mut component_ids = BTreeSet::new();
    let mut component_count = 0usize;
    while let Some((component, depth)) = stack.pop() {
        component_count = component_count.saturating_add(1);
        if component_count > APP_SURFACE_MAX_DECLARATIVE_COMPONENTS {
            return Err(AppManifestError::InvalidManifest(format!(
                "view `{view_name}` declares more than {} surface components",
                APP_SURFACE_MAX_DECLARATIVE_COMPONENTS
            )));
        }
        if depth > APP_SURFACE_MAX_DECLARATIVE_DEPTH {
            return Err(AppManifestError::InvalidManifest(format!(
                "view `{view_name}` surface components exceed depth {}",
                APP_SURFACE_MAX_DECLARATIVE_DEPTH
            )));
        }
        let (id, fields, children): (
            &AppName,
            Option<&[AppName]>,
            Option<&[AppManifestSurfaceComponent]>,
        ) = match component {
            AppManifestSurfaceComponent::Detail { id, fields }
            | AppManifestSurfaceComponent::Form { id, fields }
            | AppManifestSurfaceComponent::List { id, fields } => {
                (id, Some(fields.as_slice()), None)
            },
            AppManifestSurfaceComponent::Table { id, columns } => {
                (id, Some(columns.as_slice()), None)
            },
            AppManifestSurfaceComponent::Section { id, children } => {
                (id, None, Some(children.as_slice()))
            },
        };
        if !component_ids.insert(id.clone()) {
            return Err(AppManifestError::InvalidManifest(format!(
                "view `{view_name}` repeats surface component id `{id}`"
            )));
        }
        if let Some(fields) = fields {
            if fields.is_empty() || fields.len() > limits.max_fields {
                return Err(AppManifestError::InvalidManifest(format!(
                    "view `{view_name}` component `{id}` must bind between 1 and {} fields",
                    limits.max_fields
                )));
            }
            validate_unique_names("view.components.fields", fields)?;
            for field in fields {
                if !entity.fields.contains_key(field) {
                    return Err(AppManifestError::InvalidManifest(format!(
                        "view `{view_name}` component `{id}` references missing field `{field}`"
                    )));
                }
            }
        }
        if let Some(children) = children {
            if children.is_empty() {
                return Err(AppManifestError::InvalidManifest(format!(
                    "view `{view_name}` section `{id}` must contain at least one component"
                )));
            }
            stack.extend(
                children
                    .iter()
                    .rev()
                    .map(|child| (child, depth.saturating_add(1))),
            );
        }
    }
    Ok(())
}

fn validate_nonempty_unique_values<T: Eq + Ord>(
    field: &'static str,
    values: &[T],
    maximum: usize,
) -> Result<(), AppManifestError> {
    if values.is_empty() {
        return Err(AppManifestError::InvalidManifest(format!(
            "{field} must not be empty"
        )));
    }
    validate_vec_limit(field, values.len(), maximum)?;
    let unique = values.iter().collect::<BTreeSet<_>>();
    if unique.len() != values.len() {
        return Err(AppManifestError::DuplicateNormalizedValue { field });
    }
    Ok(())
}

fn validate_policy_override(
    policy: &AppManifestPolicyOverride,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    validate_vec_limit(
        "approved_destinations",
        policy.approved_destinations.len(),
        limits.max_dependencies,
    )?;
    validate_unique_refs("approved_destinations", &policy.approved_destinations)?;
    match policy.external_egress {
        Some(AppExternalEgress::Denied) if !policy.approved_destinations.is_empty() => {
            Err(AppManifestError::InvalidManifest(
                "a denied egress override cannot approve destinations".to_owned(),
            ))
        },
        Some(AppExternalEgress::ApprovedDestinations)
            if policy.approved_destinations.is_empty() =>
        {
            Err(AppManifestError::InvalidManifest(
                "an approved egress override requires destinations".to_owned(),
            ))
        },
        _ => Ok(()),
    }
}

fn validate_resources(resources: &AppManifestResources) -> Result<(), AppManifestError> {
    if resources.per_run.max_tokens == 0
        || resources.per_run.max_cost_usd.microusd() == 0
        || resources.per_run.max_active_seconds == 0
        || resources.monthly.max_tokens == 0
        || resources.monthly.max_cost_usd.microusd() == 0
        || resources.storage.max_records == 0
        || resources.storage.max_bytes == 0
    {
        return Err(AppManifestError::InvalidManifest(
            "all requested resource ceilings must be positive".to_owned(),
        ));
    }
    if resources.per_run.max_tokens > resources.monthly.max_tokens
        || resources.per_run.max_cost_usd > resources.monthly.max_cost_usd
    {
        return Err(AppManifestError::InvalidManifest(
            "per-run token/cost ceilings cannot exceed monthly ceilings".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_referenced_members_exist(
    manifest: &AppPackageManifest,
    members: &[AppValidatedBundleMember],
) -> Result<(), AppManifestError> {
    let existing = members
        .iter()
        .map(|member| &member.path)
        .collect::<BTreeSet<_>>();
    for path in manifest
        .app
        .workflows
        .values()
        .map(|workflow| &workflow.prompt)
        .chain(
            manifest
                .app
                .workflows
                .values()
                .filter_map(|workflow| workflow.recipe.as_ref()),
        )
        .chain(manifest.app.assets.iter())
        .chain(
            manifest
                .app
                .dependencies
                .procedure_skills
                .iter()
                .filter_map(|dependency| dependency.vendored_path.as_ref()),
        )
    {
        if !existing.contains(path) {
            return Err(AppManifestError::MissingBundleMember(path.to_string()));
        }
    }
    Ok(())
}

fn validate_named_map<T>(
    field: &'static str,
    values: &BTreeMap<AppName, T>,
    limit: usize,
) -> Result<(), AppManifestError> {
    if values.len() > limit {
        return Err(AppManifestError::CollectionLimit { field, limit });
    }
    let mut normalized = BTreeSet::new();
    for name in values.keys() {
        if !normalized.insert(normalized_collision_key(name.as_str())) {
            return Err(AppManifestError::DuplicateNormalizedIdentifier(
                name.to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_vec_limit(
    field: &'static str,
    len: usize,
    limit: usize,
) -> Result<(), AppManifestError> {
    if len > limit {
        return Err(AppManifestError::CollectionLimit { field, limit });
    }
    Ok(())
}

fn validate_unique_names(field: &'static str, values: &[AppName]) -> Result<(), AppManifestError> {
    let mut normalized = BTreeSet::new();
    for name in values {
        if !normalized.insert(normalized_collision_key(name.as_str())) {
            return Err(AppManifestError::DuplicateNormalizedValue { field });
        }
    }
    Ok(())
}

fn validate_unique_refs(
    field: &'static str,
    values: &[AppReference],
) -> Result<(), AppManifestError> {
    let mut normalized = BTreeSet::new();
    for reference in values {
        if !normalized.insert(normalized_collision_key(reference.as_str())) {
            return Err(AppManifestError::DuplicateNormalizedValue { field });
        }
    }
    Ok(())
}

fn validate_unique_paths(
    field: &'static str,
    values: &[AppBundlePath],
    limit: usize,
) -> Result<(), AppManifestError> {
    if values.len() > limit {
        return Err(AppManifestError::CollectionLimit { field, limit });
    }
    let mut normalized = BTreeSet::new();
    for path in values {
        if !normalized.insert(path.collision_key()) {
            return Err(AppManifestError::DuplicateNormalizedValue { field });
        }
    }
    Ok(())
}

fn validate_version_requirement(
    requirement: &str,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    if requirement.is_empty() || requirement.len() > limits.max_string_bytes {
        return Err(AppManifestError::InvalidManifest(
            "version requirement is empty or too large".to_owned(),
        ));
    }
    semver::VersionReq::parse(requirement).map_err(|error| {
        AppManifestError::InvalidManifest(format!("invalid version requirement: {error}"))
    })?;
    Ok(())
}

fn extract_frontmatter<'a>(
    source: &'a [u8],
    max_manifest_bytes: usize,
) -> Result<&'a [u8], AppManifestError> {
    if source.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(AppManifestError::InvalidFrontmatter(
            "UTF-8 BOM is not permitted".to_owned(),
        ));
    }
    let first_newline = source
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| {
            AppManifestError::InvalidFrontmatter("opening delimiter missing".to_owned())
        })?;
    let opening = source[..first_newline]
        .strip_suffix(b"\r")
        .unwrap_or(&source[..first_newline]);
    if opening != b"---" {
        return Err(AppManifestError::InvalidFrontmatter(
            "document must begin with an exact `---` line".to_owned(),
        ));
    }
    let start = first_newline.saturating_add(1);
    let mut cursor = start;
    while cursor <= source.len() {
        let relative_end = source[cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(source.len().saturating_sub(cursor));
        let end = cursor.saturating_add(relative_end);
        let line = source[cursor..end]
            .strip_suffix(b"\r")
            .unwrap_or(&source[cursor..end]);
        if line == b"---" {
            let yaml = &source[start..cursor];
            if yaml.len() > max_manifest_bytes {
                return Err(AppManifestError::ManifestTooLarge {
                    limit: max_manifest_bytes,
                });
            }
            return Ok(yaml);
        }
        if end == source.len() {
            break;
        }
        cursor = end.saturating_add(1);
        if cursor.saturating_sub(start) > max_manifest_bytes {
            return Err(AppManifestError::ManifestTooLarge {
                limit: max_manifest_bytes,
            });
        }
    }
    Err(AppManifestError::InvalidFrontmatter(
        "closing delimiter missing".to_owned(),
    ))
}

pub(crate) fn preflight_manifest_yaml(
    yaml: &[u8],
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    if yaml.len() > limits.max_manifest_bytes {
        return Err(AppManifestError::ManifestTooLarge {
            limit: limits.max_manifest_bytes,
        });
    }
    std::str::from_utf8(yaml)
        .map_err(|_| AppManifestError::InvalidYaml("manifest is not UTF-8".to_owned()))?;
    if yaml.contains(&0) {
        return Err(AppManifestError::UnsafeYamlSyntax(
            "NUL bytes are forbidden".to_owned(),
        ));
    }

    let mut flow_stack = Vec::<u8>::new();
    let mut nodes = 1usize;
    for raw_line in yaml.split(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.contains(&b'\t') {
            return Err(AppManifestError::UnsafeYamlSyntax(
                "tabs are forbidden; use two-space indentation".to_owned(),
            ));
        }
        let indent = line.iter().take_while(|byte| **byte == b' ').count();
        if indent % 2 != 0 {
            return Err(AppManifestError::UnsafeYamlSyntax(
                "indentation must use multiples of two spaces".to_owned(),
            ));
        }
        if indent / 2 + flow_stack.len() + 1 > limits.max_yaml_depth {
            return Err(AppManifestError::YamlDepthLimit {
                limit: limits.max_yaml_depth,
            });
        }
        let trimmed = &line[indent..];
        if trimmed.starts_with(b"%") || trimmed == b"---" || trimmed == b"..." {
            return Err(AppManifestError::UnsafeYamlSyntax(
                "directives and nested YAML documents are forbidden".to_owned(),
            ));
        }
        scan_yaml_line(trimmed, indent / 2 + 1, &mut flow_stack, &mut nodes, limits)?;
    }
    if !flow_stack.is_empty() {
        return Err(AppManifestError::UnsafeYamlSyntax(
            "unclosed flow collection".to_owned(),
        ));
    }
    Ok(())
}

/// Agent definitions share the app catalog's bounded lexical YAML admission,
/// but their established codec legitimately uses the exact `!days` and
/// `!fixed` scalar tags plus `|`/`>` block scalars for bounded persona and
/// template text. Cron expressions also use a standalone `*`, which cannot be
/// a YAML alias because it has no alias name. Build a lexical-only copy that
/// masks those closed scalar tokens and embedded alphanumeric apostrophes, then
/// replaces the six block-scalar indicators with an empty quoted scalar while
/// omitting their indented text. The original bytes still go through the
/// canonical `AgentDefinition` decoder and its per-field limits; quote
/// delimiters, named aliases, anchors, merge keys, arbitrary tags, tabs and
/// malformed structural YAML remain rejected before Serde sees them. App
/// manifests do not use this compatibility preflight and retain their smaller
/// YAML subset.
pub(crate) fn preflight_agent_definition_yaml(
    yaml: &[u8],
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    const ALLOWED_TAGS: [&[u8]; 2] = [b"!days", b"!fixed"];
    const BLOCK_SCALARS: [&[u8]; 6] = [b"|", b"|-", b"|+", b">", b">-", b">+"];
    if yaml.len() > limits.max_manifest_bytes {
        return Err(AppManifestError::ManifestTooLarge {
            limit: limits.max_manifest_bytes,
        });
    }
    std::str::from_utf8(yaml)
        .map_err(|_| AppManifestError::InvalidYaml("agent definition is not UTF-8".to_owned()))?;
    if yaml.contains(&0) {
        return Err(AppManifestError::UnsafeYamlSyntax(
            "NUL bytes are forbidden".to_owned(),
        ));
    }
    if yaml.contains(&b'\t') {
        return Err(AppManifestError::UnsafeYamlSyntax(
            "tabs are forbidden; use two-space indentation".to_owned(),
        ));
    }

    let mut lexical = Vec::with_capacity(yaml.len());
    let mut block_parent_indent = None;
    for raw_line in yaml.split(|byte| *byte == b'\n') {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let indent = line.iter().take_while(|byte| **byte == b' ').count();
        let blank = line.iter().all(u8::is_ascii_whitespace);
        if let Some(parent_indent) = block_parent_indent {
            if blank || indent > parent_indent {
                lexical.push(b'\n');
                continue;
            }
            block_parent_indent = None;
        }

        let block_indicator = line.iter().enumerate().find_map(|(index, byte)| {
            if *byte != b':' {
                return None;
            }
            let suffix = line[index.saturating_add(1)..]
                .iter()
                .copied()
                .skip_while(u8::is_ascii_whitespace)
                .collect::<Vec<_>>();
            BLOCK_SCALARS
                .iter()
                .any(|indicator| suffix.as_slice() == *indicator)
                .then_some(index)
        });
        if let Some(colon) = block_indicator {
            lexical.extend_from_slice(&line[..=colon]);
            lexical.extend_from_slice(b" \"\"");
            lexical.push(b'\n');
            block_parent_indent = Some(indent);
            continue;
        }

        let start = lexical.len();
        lexical.extend_from_slice(line);
        let mut index = 0usize;
        // Whether an apostrophe is a quote delimiter or ordinary prose depends
        // on where it sits, so this tracks single-quote state across the line.
        //
        // The earlier rule neutralized `'` only when letters flanked it on BOTH
        // sides, which reads `don't` correctly but not `contacts' preferred` --
        // an ordinary possessive plural, and valid YAML, that was rejected as
        // an unclosed quoted scalar. A quote can only OPEN a scalar at a value
        // boundary; one preceded by a word character while no scalar is open is
        // prose. The unterminated-quote defence is unchanged: a scalar opened at
        // a boundary and never closed still leaves the quote unbalanced for the
        // structural pass below.
        let mut inside_single_quote = false;
        while index < line.len() {
            if line[index] == b'\'' {
                if inside_single_quote {
                    // YAML escapes a quote inside a quoted scalar by doubling
                    // it; `'it''s'` is one scalar, not two.
                    if line.get(index.saturating_add(1)) == Some(&b'\'') {
                        index = index.saturating_add(2);
                        continue;
                    }
                    inside_single_quote = false;
                    index = index.saturating_add(1);
                    continue;
                }
                let opens_a_scalar = index == 0
                    || line[index - 1].is_ascii_whitespace()
                    || matches!(line[index - 1], b'[' | b'{' | b',' | b':' | b'-');
                if opens_a_scalar {
                    inside_single_quote = true;
                } else {
                    lexical[start + index] = b'x';
                }
                index = index.saturating_add(1);
                continue;
            }
            // A `*` only begins an alias where a node begins. Mid-way through
            // a plain scalar it is text: `schedule: 40 */2 * * *` is a cron
            // string, and serde_yaml writes crons unquoted, so rejecting it
            // made Magician's own saved agent definitions fail preflight. A
            // `*` whose preceding non-space byte is ordinary scalar text is
            // therefore neutral; one after `:` `-` `?` `[` `{` `,` or at the
            // start of the line still reaches the alias check.
            let mid_plain_scalar = index > 0
                && line[index - 1].is_ascii_whitespace()
                && line[..index]
                    .iter()
                    .rev()
                    .find(|byte| !byte.is_ascii_whitespace())
                    .is_some_and(|byte| !matches!(byte, b':' | b'-' | b'?' | b'[' | b'{' | b','));
            if line[index] == b'*'
                && (mid_plain_scalar
                    || ((index == 0 || line[index - 1].is_ascii_whitespace())
                        && line
                            .get(index.saturating_add(1))
                            .is_none_or(u8::is_ascii_whitespace)))
            {
                lexical[start + index] = b'x';
                index = index.saturating_add(1);
                continue;
            }
            if line[index] != b'!' {
                index = index.saturating_add(1);
                continue;
            }
            let previous_is_boundary = index == 0
                || line[index - 1].is_ascii_whitespace()
                || matches!(line[index - 1], b'[' | b'{' | b',' | b':' | b'-');
            if !previous_is_boundary {
                index = index.saturating_add(1);
                continue;
            }
            let allowed = ALLOWED_TAGS.iter().find(|tag| {
                line[index..].starts_with(tag)
                    && line
                        .get(index.saturating_add(tag.len()))
                        .is_none_or(u8::is_ascii_whitespace)
            });
            let Some(tag) = allowed else {
                index = index.saturating_add(1);
                continue;
            };
            lexical[start + index..start + index + tag.len()].fill(b'x');
            index = index.saturating_add(tag.len());
        }
        lexical.push(b'\n');
    }
    preflight_manifest_yaml(&lexical, limits)
}

/// Enforce exact decoded YAML ceilings after the lexical preflight has ruled
/// out aliases, tags and excessive nesting. This walker is iterative so a
/// hostile manifest cannot turn validation into recursive call-stack growth.
fn validate_yaml_value_limits(
    root: &serde_yaml::Value,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    let mut nodes = 0usize;
    let mut stack = vec![(root, 1usize)];
    while let Some((value, depth)) = stack.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > limits.max_yaml_nodes {
            return Err(AppManifestError::YamlNodeLimit {
                limit: limits.max_yaml_nodes,
            });
        }
        if depth > limits.max_yaml_depth {
            return Err(AppManifestError::YamlDepthLimit {
                limit: limits.max_yaml_depth,
            });
        }
        match value {
            serde_yaml::Value::String(text) => {
                if text.len() > limits.max_string_bytes {
                    return Err(AppManifestError::YamlStringLimit {
                        limit: limits.max_string_bytes,
                    });
                }
            },
            serde_yaml::Value::Sequence(values) => {
                stack.extend(
                    values
                        .iter()
                        .rev()
                        .map(|value| (value, depth.saturating_add(1))),
                );
            },
            serde_yaml::Value::Mapping(values) => {
                for (key, value) in values {
                    stack.push((value, depth.saturating_add(1)));
                    stack.push((key, depth.saturating_add(1)));
                }
            },
            serde_yaml::Value::Tagged(tagged) => {
                stack.push((&tagged.value, depth.saturating_add(1)));
            },
            serde_yaml::Value::Null | serde_yaml::Value::Bool(_) | serde_yaml::Value::Number(_) => {
            },
        }
    }
    Ok(())
}

fn scan_yaml_line(
    line: &[u8],
    base_depth: usize,
    flow_stack: &mut Vec<u8>,
    nodes: &mut usize,
    limits: &AppPackageLimits,
) -> Result<(), AppManifestError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Quote {
        Plain,
        Single,
        Double,
    }
    let mut quote = Quote::Plain;
    let mut escaped = false;
    let mut token_bytes = 0usize;
    let mut token_started = false;
    // YAML permits compact block nesting such as `- - - value` on one line.
    // Serde must never see more nesting than the contract admits, so account
    // for those implicit containers before decoding. `mapping_value_depth`
    // represents the child of a block mapping (`key: value`); flow mappings
    // are already represented by `flow_stack` and must not be double-counted.
    let mut compact_block_depth = 0usize;
    let mut mapping_value_depth = 0usize;
    let mut index = 0usize;
    while index < line.len() {
        let byte = line[index];
        match quote {
            Quote::Single => {
                if byte == b'\'' {
                    if line.get(index + 1) == Some(&b'\'') {
                        index = index.saturating_add(1);
                    } else {
                        quote = Quote::Plain;
                    }
                }
                token_bytes = token_bytes.saturating_add(1);
            },
            Quote::Double => {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quote = Quote::Plain;
                }
                token_bytes = token_bytes.saturating_add(1);
            },
            Quote::Plain => {
                let previous_is_boundary = index == 0
                    || line[index - 1].is_ascii_whitespace()
                    || matches!(line[index - 1], b'[' | b'{' | b',' | b':' | b'-');
                if byte == b'#' && (index == 0 || line[index - 1].is_ascii_whitespace()) {
                    break;
                }
                if byte == b'\'' {
                    quote = Quote::Single;
                    token_bytes = 0;
                    token_started = true;
                } else if byte == b'"' {
                    quote = Quote::Double;
                    token_bytes = 0;
                    token_started = true;
                } else if matches!(byte, b'&' | b'*' | b'!') && previous_is_boundary {
                    return Err(AppManifestError::YamlAliasOrTagForbidden);
                } else if (byte == b'|' || byte == b'>') && previous_is_boundary {
                    return Err(AppManifestError::UnsafeYamlSyntax(
                        "block scalars are not part of the app-manifest YAML subset".to_owned(),
                    ));
                } else if byte == b'<' && line.get(index + 1) == Some(&b'<') {
                    return Err(AppManifestError::YamlAliasOrTagForbidden);
                } else if matches!(byte, b'[' | b'{') {
                    flow_stack.push(byte);
                    *nodes = nodes.saturating_add(1);
                    token_bytes = 0;
                    token_started = false;
                } else if matches!(byte, b']' | b'}') {
                    let expected = if byte == b']' { b'[' } else { b'{' };
                    if flow_stack.pop() != Some(expected) {
                        return Err(AppManifestError::UnsafeYamlSyntax(
                            "mismatched flow collection".to_owned(),
                        ));
                    }
                    token_bytes = 0;
                    token_started = false;
                } else if byte == b',' {
                    *nodes = nodes.saturating_add(1);
                    token_bytes = 0;
                    token_started = false;
                } else if byte == b':' {
                    // Count both the mapping key and value conservatively. This
                    // also covers compact JSON-style YAML (`"k":"v"`) that
                    // the previous whitespace-sensitive count under-reported.
                    *nodes = nodes.saturating_add(2);
                    if flow_stack.is_empty()
                        && (line.get(index + 1).is_none() || line[index + 1].is_ascii_whitespace())
                    {
                        mapping_value_depth = 1;
                    }
                    token_bytes = 0;
                    token_started = false;
                } else if matches!(byte, b'-' | b'?')
                    && previous_is_boundary
                    && (line.get(index + 1).is_none() || line[index + 1].is_ascii_whitespace())
                {
                    *nodes = nodes.saturating_add(1);
                    if !token_started && flow_stack.is_empty() {
                        compact_block_depth = compact_block_depth.saturating_add(1);
                    }
                    token_bytes = 0;
                    token_started = false;
                } else if byte.is_ascii_whitespace() {
                    if token_started {
                        token_bytes = token_bytes.saturating_add(1);
                    }
                } else {
                    token_bytes = token_bytes.saturating_add(1);
                    token_started = true;
                }
            },
        }
        let lexical_depth = base_depth
            .saturating_add(compact_block_depth)
            .saturating_add(mapping_value_depth)
            .saturating_add(flow_stack.len());
        if lexical_depth > limits.max_yaml_depth {
            return Err(AppManifestError::YamlDepthLimit {
                limit: limits.max_yaml_depth,
            });
        }
        if *nodes > limits.max_yaml_nodes {
            return Err(AppManifestError::YamlNodeLimit {
                limit: limits.max_yaml_nodes,
            });
        }
        if token_bytes > limits.max_string_bytes {
            return Err(AppManifestError::YamlStringLimit {
                limit: limits.max_string_bytes,
            });
        }
        index = index.saturating_add(1);
    }
    if quote != Quote::Plain {
        return Err(AppManifestError::UnsafeYamlSyntax(
            "quoted scalars must close on the same line".to_owned(),
        ));
    }
    Ok(())
}

fn normalize_bundle_path(raw: &str, max_bytes: usize) -> Result<String, AppManifestError> {
    if raw.is_empty() || raw.len() > max_bytes || raw.chars().any(char::is_control) {
        return Err(AppManifestError::UnsafeBundlePath(raw.to_owned()));
    }
    let normalized = raw.nfkc().collect::<String>();
    // V1 package-internal paths deliberately use one portable ASCII alphabet.
    // NFKC is applied first so compatibility characters cannot normalize into
    // a platform separator, drive/stream colon, absolute prefix or ignored
    // collision after the safety checks have already run.
    if normalized.is_empty()
        || normalized.len() > max_bytes
        || !normalized.is_ascii()
        || normalized.starts_with('/')
        || normalized.contains('\\')
        || normalized.contains(':')
        || normalized.chars().any(char::is_control)
    {
        return Err(AppManifestError::UnsafeBundlePath(raw.to_owned()));
    }
    let mut segment_count = 0usize;
    for segment in normalized.split('/') {
        segment_count = segment_count.saturating_add(1);
        if segment.is_empty()
            || matches!(segment, "." | "..")
            || segment.starts_with(char::is_whitespace)
            || segment.ends_with(char::is_whitespace)
            || segment.len() > 128
        {
            return Err(AppManifestError::UnsafeBundlePath(raw.to_owned()));
        }
    }
    if segment_count > 32 || normalized.len() > max_bytes {
        return Err(AppManifestError::UnsafeBundlePath(raw.to_owned()));
    }
    Ok(normalized)
}

fn normalize_route(raw: &str) -> Result<String, AppManifestError> {
    if raw.len() > MAX_ROUTE_BYTES {
        return Err(AppManifestError::UnsafeRoute(raw.to_owned()));
    }
    let normalized = raw.nfkc().collect::<String>();
    if normalized.is_empty()
        || normalized.len() > MAX_ROUTE_BYTES
        || !normalized.is_ascii()
        || !normalized.starts_with('/')
        || normalized
            .bytes()
            .any(|byte| matches!(byte, b'\\' | b'?' | b'#' | b'%'))
        || normalized.chars().any(char::is_control)
    {
        return Err(AppManifestError::UnsafeRoute(raw.to_owned()));
    }
    if normalized == "/" {
        return Ok(normalized);
    }
    let segments = normalized[1..].split('/').collect::<Vec<_>>();
    if segments.is_empty() || segments.len() > 16 {
        return Err(AppManifestError::UnsafeRoute(raw.to_owned()));
    }
    let mut parameters = BTreeSet::new();
    for (index, segment) in segments.iter().enumerate() {
        if segment.is_empty() || matches!(*segment, "." | "..") {
            return Err(AppManifestError::UnsafeRoute(raw.to_owned()));
        }
        if index == 0
            && matches!(
                normalized_collision_key(segment).as_str(),
                "api" | "_app" | ".well-known"
            )
        {
            return Err(AppManifestError::HostGlobalRoute(raw.to_owned()));
        }
        if let Some(parameter) = segment.strip_prefix(':') {
            let parameter = AppName::parse(parameter)
                .map_err(|_| AppManifestError::UnsafeRoute(raw.to_owned()))?;
            if !parameters.insert(normalized_collision_key(parameter.as_str())) {
                return Err(AppManifestError::UnsafeRoute(raw.to_owned()));
            }
        } else if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(AppManifestError::UnsafeRoute(raw.to_owned()));
        }
    }
    Ok(normalized)
}

/// Return true when two admitted templates can resolve the same concrete URL.
///
/// Parameter names are documentation, not routing discriminators. A static
/// segment also overlaps a parameter in the same position, so `/items/new`
/// and `/items/:item_id` cannot coexist without introducing order-dependent
/// authority resolution.
pub fn app_route_templates_overlap(left: &str, right: &str) -> bool {
    if left == "/" || right == "/" {
        return left == right;
    }
    let left_segments = left[1..].split('/').collect::<Vec<_>>();
    let right_segments = right[1..].split('/').collect::<Vec<_>>();
    left_segments.len() == right_segments.len()
        && left_segments
            .iter()
            .zip(right_segments)
            .all(|(left, right)| {
                left.starts_with(':')
                    || right.starts_with(':')
                    || normalized_collision_key(left) == normalized_collision_key(right)
            })
}

pub fn normalized_collision_key(raw: &str) -> String {
    // Every identifier/reference and admitted bundle/static-route component is
    // ASCII by construction. Keeping collision keys in that same alphabet
    // avoids incomplete Unicode lowercase mappings masquerading as case-folding.
    raw.nfkc().collect::<String>().to_ascii_lowercase()
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppManifestError {
    #[error("app manifest exceeds the {limit} byte ceiling")]
    ManifestTooLarge { limit: usize },
    #[error("invalid app frontmatter: {0}")]
    InvalidFrontmatter(String),
    #[error("invalid app manifest YAML: {0}")]
    InvalidYaml(String),
    #[error("YAML aliases, anchors, tags and merge keys are forbidden")]
    YamlAliasOrTagForbidden,
    #[error("unsafe app-manifest YAML syntax: {0}")]
    UnsafeYamlSyntax(String),
    #[error("app manifest exceeds the {limit} level YAML depth ceiling")]
    YamlDepthLimit { limit: usize },
    #[error("app manifest exceeds the {limit} YAML node ceiling")]
    YamlNodeLimit { limit: usize },
    #[error("app manifest scalar exceeds the {limit} byte ceiling")]
    YamlStringLimit { limit: usize },
    #[error("invalid app manifest: {0}")]
    InvalidManifest(String),
    #[error("unsupported app manifest version `{0}`")]
    UnsupportedManifestVersion(String),
    #[error("unsupported app authoring SDK version `{0}`")]
    UnsupportedSdkVersion(String),
    #[error("unsupported required app-manifest feature `{0}`")]
    UnsupportedManifestFeature(String),
    #[error("app view `{view}` uses unsupported V1 view kind `{kind}`")]
    UnsupportedViewKind { view: String, kind: &'static str },
    #[error("app manifest `{field}` exceeds the {limit} item ceiling")]
    CollectionLimit { field: &'static str, limit: usize },
    #[error("app manifest exceeds the {limit} total field ceiling")]
    FieldLimit { limit: usize },
    #[error("app dependency set exceeds the {limit} item ceiling")]
    DependencyLimit { limit: usize },
    #[error("duplicate identifier after normalization: `{0}`")]
    DuplicateNormalizedIdentifier(String),
    #[error("app manifest `{field}` contains a duplicate normalized value")]
    DuplicateNormalizedValue { field: &'static str },
    #[error("duplicate app route after normalization: `{0}`")]
    DuplicateRoute(String),
    #[error("unsafe app route `{0}`")]
    UnsafeRoute(String),
    #[error("app route collides with a host-global namespace: `{0}`")]
    HostGlobalRoute(String),
    #[error("unsafe app bundle path `{0}`")]
    UnsafeBundlePath(String),
    #[error("duplicate app bundle path after Unicode/case normalization: `{0}`")]
    DuplicateNormalizedPath(String),
    #[error("app bundle must contain between 1 and {limit} files")]
    BundleFileLimit { limit: usize },
    #[error("app bundle member `{path}` exceeds the {limit} byte ceiling")]
    BundleMemberTooLarge { path: String, limit: usize },
    #[error("failed to read app bundle member `{path}`: {message}")]
    BundleRead { path: String, message: String },
    #[error("app bundle member `{path}` declared {declared} bytes but yielded {actual} bytes")]
    BundleMemberLengthMismatch {
        path: String,
        declared: u64,
        actual: u64,
    },
    #[error("app bundle exceeds the {limit} byte ceiling")]
    BundleTooLarge { limit: usize },
    #[error("symlink member `{0}` is forbidden")]
    SymlinkMember(String),
    #[error("special-file member `{0}` is forbidden")]
    SpecialMember(String),
    #[error("app bundle has no canonical SKILL.md manifest")]
    MissingSkillManifest,
    #[error("app bundle is missing referenced member `{0}`")]
    MissingBundleMember(String),
    #[error("failed to encode canonical app identity: {0}")]
    CanonicalEncoding(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod tests {

    /// The shipped Town Square manifest and the recipe rules, driven against
    /// the real bytes rather than a fixture that can drift from them.
    ///
    /// `operations` is an unordered allow-set. Until this contract existed the
    /// LLM dispatcher was deliberately unreachable, because executing an
    /// allow-set means guessing an order. These are the rules that make
    /// executing one safe, so each is asserted by its refusal.
    mod behavior_recipe {
        use super::*;

        fn town_square_manifest() -> String {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../magician_data_v3/system/town_square/app/SKILL.md");
            std::fs::read_to_string(path).expect("shipped town square manifest")
        }

        fn parse(source: &str) -> Result<CanonicalAppManifest, AppManifestError> {
            parse_app_manifest_frontmatter(
                source.as_bytes(),
                &AppPackageLimits::for_bounded_yaml(256 * 1024),
            )
        }

        // Guard parsing remains covered with an explicit auto-runner recipe;
        // the shipped native round now has one unguarded semantic step.
        fn guarded_manifest() -> String {
            let source = town_square_manifest();
            let yaml = extract_frontmatter(source.as_bytes(), 256 * 1024).unwrap();
            let mut value: serde_json::Value = serde_yaml::from_slice(yaml).unwrap();
            let app = &mut value["app"];
            app["llm_operations"]["engagement_gate"] =
                app["llm_operations"]["compose_post"].clone();
            let behavior = &mut app["behaviors"][0];
            behavior["operations"] = serde_json::json!(["engagement_gate", "compose_post"]);
            let compose = serde_json::json!({
                "id":"compose", "operation":"compose_post",
                "when":{"step":"gate", "field":"decision", "equals":"engage"},
                "output_schema":{"type":"object", "fields":{"body":{"type":"text", "required":true}}}
            });
            behavior["steps"] = serde_json::json!([
                {"id":"gate", "operation":"engagement_gate", "output_schema":{
                    "type":"object", "fields":{"decision":{"type":"enum", "values":["engage","pass"], "required":true}}
                }}, compose
            ]);
            app["workflows"]["take_ambient_turn"]["runner"] = serde_json::json!("auto");
            app["workflows"]["take_ambient_turn"]["result"] = serde_json::json!({
                "kind":"entity_projection", "entities":["post", "mention", "self_state", "turn_cursor"]
            });
            app["workflows"]["take_ambient_turn"]
                .as_object_mut()
                .unwrap()
                .remove("recipe");
            let fixture = format!(
                "---\n{}---\nGuard validation fixture.\n",
                serde_yaml::to_string(&value).unwrap()
            );
            parse(&fixture).expect("guard fixture is valid before mutation");
            fixture
        }

        fn mutated(from: &str, to: &str) -> String {
            let source = guarded_manifest();
            assert!(
                source.contains(from),
                "guard fixture does not contain {from}"
            );
            source.replacen(from, to, 1)
        }

        #[test]
        fn the_shipped_recipe_has_one_reviewed_compose_step() {
            let manifest = parse(&town_square_manifest()).expect("shipped manifest is valid");
            let behavior = &manifest.manifest().app.behaviors[0];
            assert_eq!(behavior.steps.len(), 1);
            assert_eq!(behavior.steps[0].id.as_str(), "compose");
            assert_eq!(behavior.steps[0].operation.as_str(), "compose_post");
            assert!(behavior.steps[0].when.is_none());
            assert!(behavior.steps[0].output_schema.value_schema.is_some());
            assert!(behavior.output_schema.is_none());
        }

        #[test]
        fn a_guard_cannot_name_a_later_step() {
            let source = mutated("step: gate", "step: compose");
            let error = parse(&source).expect_err("a self reference must be refused");
            assert!(
                error.to_string().contains("not an earlier step"),
                "unexpected error: {error}"
            );
        }

        #[test]
        fn a_guard_value_must_be_one_the_field_can_take() {
            let source = mutated("equals: engage", "equals: engaged");
            let error = parse(&source).expect_err("an enum typo must be refused");
            assert!(
                error.to_string().contains("is not one of field"),
                "unexpected error: {error}"
            );
        }

        #[test]
        fn a_guard_cannot_read_a_field_the_source_step_does_not_produce() {
            let source = mutated("field: decision", "field: post_type");
            let error = parse(&source).expect_err("guarding an absent field must be refused");
            assert!(
                error
                    .to_string()
                    .contains("does not \n                     produce")
                    || error.to_string().contains("does not"),
                "unexpected error: {error}"
            );
        }

        #[test]
        fn a_step_cannot_name_an_operation_outside_the_allow_set() {
            let source = mutated("operation: compose_post\n", "operation: summarise_feed\n");
            let error = parse(&source).expect_err("an unlisted operation must be refused");
            assert!(
                error.to_string().contains("not in its reviewed"),
                "unexpected error: {error}"
            );
        }

        /// The rule the whole contract exists for.
        #[test]
        fn several_operations_without_a_recipe_are_refused() {
            let source = guarded_manifest();
            let yaml = extract_frontmatter(source.as_bytes(), 256 * 1024).unwrap();
            let mut value: serde_json::Value = serde_yaml::from_slice(yaml).unwrap();
            value["app"]["behaviors"][0]
                .as_object_mut()
                .unwrap()
                .remove("steps");
            let stripped = format!("---\n{}---\n", serde_yaml::to_string(&value).unwrap());
            let error = parse(&stripped).expect_err("an allow-set is not a recipe");
            assert!(
                error.to_string().contains("is not a recipe"),
                "unexpected error: {error}"
            );
        }
    }

    /// Apostrophes in prose, quoted scalars, and genuinely unclosed quotes.
    ///
    /// The possessive-plural case (`contacts' preferred channels`) is valid
    /// YAML that the preflight rejected, which made a shipped agent definition
    /// invisible to the primitive catalog.
    #[test]
    fn agent_definition_preflight_reads_apostrophes_as_prose() {
        let limits = AppPackageLimits::for_bounded_agent_definition(64 * 1024);
        let ok = |body: &str| {
            preflight_agent_definition_yaml(
                format!("agent_id: a\nversion: 3\n{body}\n").as_bytes(),
                &limits,
            )
        };

        // Possessive plural -- the regression.
        ok("description: contacts' preferred channels").expect("possessive plural is prose");
        // Apostrophe inside a word, which the old rule already handled.
        ok("description: don't guess").expect("interior apostrophe is prose");
        // An ordinary quoted scalar still parses.
        ok("description: 'Recent sessions: what was done'").expect("quoted scalar");
        // YAML's doubled-quote escape stays one scalar.
        ok("description: 'it''s fine'").expect("escaped quote inside a quoted scalar");
        // Both together on one line.
        ok("description: 'contacts'' preferred channels'").expect("escaped possessive");

        // The defence this rule exists for is unchanged: a scalar opened at a
        // value boundary and never closed is still refused.
        let unclosed = ok("description: 'never closed");
        assert!(
            matches!(unclosed, Err(AppManifestError::UnsafeYamlSyntax(_))),
            "an unterminated quoted scalar must still be refused, got {unclosed:?}"
        );
    }

    /// Unquoted crons are how serde_yaml writes an agent's heartbeat
    /// schedule back to disk. `*/2` inside that plain scalar is text, not an
    /// alias; rejecting it hid every agent with an every-N-hours task (ceo,
    /// cmo, cpo, ambassador) from the primitive catalog.
    #[test]
    fn agent_definition_preflight_reads_a_mid_scalar_star_as_text() {
        let limits = AppPackageLimits::for_bounded_agent_definition(64 * 1024);
        let check = |body: &str| {
            preflight_agent_definition_yaml(
                format!("agent_id: a\nversion: 3\n{body}\n").as_bytes(),
                &limits,
            )
        };
        check("schedule: 40 */2 * * *").expect("step cron is a plain scalar");
        check("schedule: 25,55 * * * *").expect("list cron is a plain scalar");
        check("tasks:\n  - name: t\n    schedule: 50 */6 * * *").expect("nested step cron");
        check("note: see 5 *important* items").expect("emphasis mid-scalar is text");

        // A star that opens a node is still an alias and still refused.
        for alias in [
            "base: &b {x: 1}\ncopy: *b",
            "copy: *b",
            "list:\n  - *b",
            "flow: [1, *b]",
            "flow: {k: *b}",
        ] {
            let result = check(alias);
            assert!(
                matches!(result, Err(AppManifestError::YamlAliasOrTagForbidden)),
                "an alias at a node start must be refused: {alias:?} -> {result:?}"
            );
        }
    }

    fn memory_manifest(
        features: &str,
        memory: &str,
    ) -> Result<CanonicalAppManifest, AppManifestError> {
        let document = valid_skill_document()
            .replace(
                "    app_sdk_version: \"1\"",
                &format!("    app_sdk_version: \"1\"{features}"),
            )
            .replace("  entities:\n", &format!("{memory}  entities:\n"));
        parse_app_manifest_frontmatter(document.as_bytes(), &AppPackageLimits::default())
    }

    const MEMORY_BLOCK: &str = "  memory:\n    read:\n      user_tiers: [preferences, identity]\n      agents: [scribe]\n      purpose: Personalise suggestions\n";
    const MEMORY_FEATURE: &str = "\n    required_features: [app_memory_read_v1]";

    #[test]
    fn a_memory_request_needs_its_feature_and_only_readable_tiers() {
        let parsed = memory_manifest(MEMORY_FEATURE, MEMORY_BLOCK).expect("valid memory request");
        let request = parsed
            .manifest()
            .app
            .memory
            .as_ref()
            .and_then(|memory| memory.read.as_ref())
            .expect("request kept");
        assert_eq!(request.user_tiers, ["preferences", "identity"]);

        let without_feature = memory_manifest("", MEMORY_BLOCK);
        assert!(
            matches!(&without_feature, Err(AppManifestError::InvalidManifest(message)) if message.contains("app_memory_read_v1")),
            "{without_feature:?}"
        );
        for refused in [
            "  memory:\n    read:\n      user_tiers: [knowledge]\n      purpose: x\n",
            "  memory:\n    read:\n      user_tiers: [made_up]\n      purpose: x\n",
            "  memory:\n    read:\n      purpose: x\n",
            "  memory:\n    read:\n      user_tiers: [preferences]\n      purpose: x\n      write: true\n",
            "  memory: {}\n",
        ] {
            assert!(memory_manifest(MEMORY_FEATURE, refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_manifest_without_a_memory_request_serializes_no_memory_key() {
        let parsed = parse_app_manifest_frontmatter(
            valid_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .expect("fixture parses");
        let value = serde_json::to_value(parsed.manifest()).unwrap();
        assert!(
            value["app"].get("memory").is_none(),
            "an absent block must not change existing manifest digests"
        );
    }

    use std::{io::Read, thread};

    use super::*;

    pub fn valid_skill_document() -> String {
        r#"---
name: learning-plan
version: 0.1.0
description: A bounded learning plan app.
metadata:
  magician:
    skill_type: app
    app_manifest_version: "1.0"
    app_sdk_version: "1"
app:
  compatibility:
    magician_contract: "1"
  data_policy:
    defaults:
      classification_floor: ordinary
      model_processing: remote_allowed
      personal_agent_access: approved_projection
      memory_promotion: candidate_allowed
      external_egress: denied
  entities:
    plan:
      fields:
        topic: { type: text, required: true }
        status: { type: enum, values: [new, done], required: true }
  views:
    plans:
      entity: plan
      kind: list
      route: /
  workflows:
    build:
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
      trigger: user
  actions:
    create:
      workflow: build
      input_from: build.input
      result_from: build.result
  resources:
    per_run:
      max_tokens: 100
      max_cost_usd: 0.25
      max_active_seconds: 30
    monthly:
      max_tokens: 1000
      max_cost_usd: 2.50
    storage:
      max_records: 100
      max_bytes: 1048576
  dependencies:
    procedure_skills:
      - skill: skill:summarize
        version_requirement: "^2"
        vendored_path: vendor/skills/summarize/SKILL.md
    capabilities:
      - capability: content_search
        version_requirement: "^1"
  assets: [assets/icon.svg]
---
# Learning Plan
"#
        .to_owned()
    }

    pub fn valid_bundle() -> Vec<AppBundleMember> {
        vec![
            AppBundleMember::regular_file("SKILL.md", valid_skill_document().into_bytes())
                .expect("skill member"),
            AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                .expect("workflow member"),
            AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec())
                .expect("asset member"),
            AppBundleMember::regular_file(
                "vendor/skills/summarize/SKILL.md",
                b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
            )
            .expect("vendored member"),
            AppBundleMember::regular_file(
                "vendor/skills/summarize/bin/summarize.py",
                b"print('summary')\n".to_vec(),
            )
            .expect("vendored executable member"),
        ]
    }

    pub fn contribution_port_bundle() -> Vec<AppBundleMember> {
        let manifest = valid_skill_document()
            .replace(
                "    app_sdk_version: \"1\"",
                "    app_sdk_version: \"1\"\n    required_features: [contribution_ports_v1]",
            )
            .replace(
                "      may_mutate: [plan]",
                r#"      contribution_ports:
        summary_memory:
          source:
            kind: mutation_backed_entity_projection
            entity: plan
            selected_fields: [topic, status]
          destination: memory
          purposes: [learning_continuity]
          audiences: [user:owner]
          evidence_classes: [hypothesis]
          frequency: { max_proposals: 4, window_seconds: 3600 }
          maximum_retention_seconds: 604800
      may_mutate: [plan]"#,
            );
        let mut bundle = valid_bundle();
        bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .expect("manifest member")
            .bytes = manifest.into_bytes();
        bundle
    }

    #[test]
    fn strict_manifest_accepts_complete_bounded_bundle() {
        let package = build_app_package_candidate(valid_bundle(), &AppPackageLimits::default())
            .expect("valid package");
        assert_eq!(package.manifest().manifest().name.as_str(), "learning-plan");
        assert_eq!(package.members().len(), 5);
        assert!(package.bundle_digest().as_str().starts_with("blake3:"));
    }

    pub fn llm_operation_bundle() -> Vec<AppBundleMember> {
        let manifest = valid_skill_document()
            .replace(
                "    app_sdk_version: \"1\"",
                "    app_sdk_version: \"1\"\n    required_features: [llm_operations_v1]",
            )
            .replace(
                "  dependencies:",
                r#"  llm_operations:
    summarize_record:
      purpose: Summarize one admitted record for the owner.
      max_tokens: 2048
  dependencies:"#,
            );
        let mut bundle = valid_bundle();
        bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .expect("manifest member")
            .bytes = manifest.into_bytes();
        bundle
    }

    fn rewrite_skill_manifest<F>(bundle: Vec<AppBundleMember>, rewrite: F) -> Vec<AppBundleMember>
    where
        F: FnOnce(String) -> String,
    {
        let mut bundle = bundle;
        let skill = bundle
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .expect("manifest member");
        skill.bytes = rewrite(String::from_utf8(skill.bytes.clone()).unwrap()).into_bytes();
        bundle
    }

    #[test]
    fn llm_operations_require_their_feature_and_stay_bounded() {
        let package =
            build_app_package_candidate(llm_operation_bundle(), &AppPackageLimits::default())
                .expect("declared llm operations parse behind their feature");
        let operations = &package.manifest().manifest().app.llm_operations;
        assert_eq!(operations.len(), 1);
        let operation = operations
            .get(&AppName::parse("summarize_record").unwrap())
            .unwrap();
        assert_eq!(operation.max_tokens, Some(2_048));

        let missing_feature = rewrite_skill_manifest(llm_operation_bundle(), |manifest| {
            manifest.replace("    required_features: [llm_operations_v1]\n", "")
        });
        assert!(
            build_app_package_candidate(missing_feature, &AppPackageLimits::default())
                .unwrap_err()
                .to_string()
                .contains("without required feature llm_operations_v1")
        );

        let empty_purpose = rewrite_skill_manifest(llm_operation_bundle(), |manifest| {
            manifest.replace(
                "      purpose: Summarize one admitted record for the owner.",
                "      purpose: \"   \"",
            )
        });
        assert!(
            build_app_package_candidate(empty_purpose, &AppPackageLimits::default())
                .unwrap_err()
                .to_string()
                .contains("purpose must contain")
        );

        let zero_hint = rewrite_skill_manifest(llm_operation_bundle(), |manifest| {
            manifest.replace("      max_tokens: 2048", "      max_tokens: 0")
        });
        assert!(
            build_app_package_candidate(zero_hint, &AppPackageLimits::default())
                .unwrap_err()
                .to_string()
                .contains("max_tokens hint must fit the positive execution-domain range")
        );

        let unknown_field = rewrite_skill_manifest(llm_operation_bundle(), |manifest| {
            manifest.replace(
                "      max_tokens: 2048",
                "      max_tokens: 2048\n      profile: op-app-workflow-local",
            )
        });
        assert!(
            build_app_package_candidate(unknown_field, &AppPackageLimits::default())
                .unwrap_err()
                .to_string()
                .contains("unknown field")
        );
    }

    #[test]
    fn event_and_notification_features_are_independent_closed_gates() {
        let baseline = build_app_package_candidate(valid_bundle(), &AppPackageLimits::default())
            .expect("legacy manifest remains valid without item-4 features");
        assert!(baseline
            .manifest()
            .manifest()
            .app
            .event_behaviors
            .is_empty());
        assert!(baseline
            .manifest()
            .manifest()
            .app
            .workflows
            .values()
            .all(|workflow| workflow.notification_ports.is_empty()));
        let baseline_json = serde_json::to_value(baseline.manifest().manifest()).unwrap();
        assert!(baseline_json["app"].get("event_behaviors").is_none());
        assert!(baseline_json["app"]["workflows"]["build"]
            .get("notification_ports")
            .is_none());

        let event_declaration_without_feature =
            rewrite_skill_manifest(valid_bundle(), |manifest| {
                manifest.replace(
                    "  assets: [assets/icon.svg]",
                    r#"  event_behaviors:
    - id: on_completion
      purpose: Summarize a canonical completion for the owner.
      action: create
      subscription:
        source: installation_execution_terminal
        outcomes: [succeeded]
      min_interval_seconds: 60
      operations: []
  assets: [assets/icon.svg]"#,
                )
            });
        let event_error = build_app_package_candidate(
            event_declaration_without_feature,
            &AppPackageLimits::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(event_error.contains(
            "event_behaviors or their resources without required feature app_event_behaviors_v1"
        ));

        let notification_port_without_feature =
            rewrite_skill_manifest(valid_bundle(), |manifest| {
                manifest.replace(
                    "      may_mutate: [plan]",
                    r#"      notification_ports:
        owner_briefing:
          kind: briefing
          purpose: Tell the owner about one completed plan.
          severity_ceiling: warning
          max_notifications_per_period: 1
          period_seconds: 3600
          max_pending: 1
          ttl_seconds: 3600
      may_mutate: [plan]"#,
                )
            });
        let notification_error = build_app_package_candidate(
            notification_port_without_feature,
            &AppPackageLimits::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(notification_error
            .contains("notification_ports without required feature app_owner_notifications_v1"));

        for (feature, expected) in [
            (
                "app_event_behaviors_v1",
                "requires app_event_behaviors_v1 without an event behavior declaration",
            ),
            (
                "app_owner_notifications_v1",
                "requires app_owner_notifications_v1 without a notification port",
            ),
        ] {
            let feature_without_declaration = rewrite_skill_manifest(valid_bundle(), |manifest| {
                manifest.replace(
                    "    app_sdk_version: \"1\"",
                    &format!("    app_sdk_version: \"1\"\n    required_features: [{feature}]"),
                )
            });
            assert!(
                build_app_package_candidate(
                    feature_without_declaration,
                    &AppPackageLimits::default(),
                )
                .unwrap_err()
                .to_string()
                .contains(expected),
                "feature {feature} was not independently fail-closed"
            );
        }
    }

    #[test]
    fn manifests_without_llm_operations_keep_their_exact_identity() {
        // The declaration block is skipped when empty, so a package that
        // never declares an operation serializes no `llm_operations` key and
        // keeps its canonical manifest digest byte-for-byte: absent means
        // today's behavior, not a new one.
        let baseline = build_app_package_candidate(valid_bundle(), &AppPackageLimits::default())
            .expect("baseline package");
        let canonical = serde_json::to_value(baseline.manifest().manifest()).unwrap();
        assert!(canonical["app"]
            .as_object()
            .expect("canonical manifest body")
            .get("llm_operations")
            .is_none());
        assert!(baseline.manifest().manifest().app.llm_operations.is_empty());
    }

    #[test]
    fn contribution_ports_require_feature_mutation_source_and_fixed_destination() {
        let package =
            build_app_package_candidate(contribution_port_bundle(), &AppPackageLimits::default())
                .expect("closed contribution declaration");
        let workflow = package
            .manifest()
            .manifest()
            .app
            .workflows
            .get(&AppName::parse("build").unwrap())
            .unwrap();
        let port = workflow
            .contribution_ports
            .get(&AppName::parse("summary_memory").unwrap())
            .unwrap();
        assert_eq!(port.destination, AppContributionDestination::Memory);

        let mut retrieval = contribution_port_bundle();
        let skill = retrieval
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .unwrap();
        skill.bytes = String::from_utf8(skill.bytes.clone())
            .unwrap()
            .replace(
                "destination: memory",
                "destination: personal_agent_retrieval",
            )
            .replace(
                "audiences: [user:owner]",
                "audiences: [agent:personal-assistant]",
            )
            .into_bytes();
        build_app_package_candidate(retrieval, &AppPackageLimits::default())
            .expect("fixed personal-agent retrieval destination");

        let mut contradictory_audience = contribution_port_bundle();
        let skill = contradictory_audience
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .unwrap();
        skill.bytes = String::from_utf8(skill.bytes.clone())
            .unwrap()
            .replace(
                "destination: memory",
                "destination: personal_agent_retrieval",
            )
            .into_bytes();
        assert!(
            build_app_package_candidate(contradictory_audience, &AppPackageLimits::default(),)
                .unwrap_err()
                .to_string()
                .contains("audience contradicts")
        );

        let mut missing_feature = contribution_port_bundle();
        let skill = missing_feature
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .unwrap();
        skill.bytes = String::from_utf8(skill.bytes.clone())
            .unwrap()
            .replace("    required_features: [contribution_ports_v1]\n", "")
            .into_bytes();
        assert!(
            build_app_package_candidate(missing_feature, &AppPackageLimits::default())
                .unwrap_err()
                .to_string()
                .contains("required feature")
        );

        let mut substituted_field = contribution_port_bundle();
        let skill = substituted_field
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .unwrap();
        skill.bytes = String::from_utf8(skill.bytes.clone())
            .unwrap()
            .replace(
                "selected_fields: [topic, status]",
                "selected_fields: [prompt_text]",
            )
            .into_bytes();
        assert!(
            build_app_package_candidate(substituted_field, &AppPackageLimits::default())
                .unwrap_err()
                .to_string()
                .contains("undeclared entity field")
        );

        let mut typed_value = contribution_port_bundle();
        let skill = typed_value
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .unwrap();
        skill.bytes = String::from_utf8(skill.bytes.clone())
            .unwrap()
            .replace("kind: entity_projection", "kind: typed_value")
            .into_bytes();
        assert!(
            build_app_package_candidate(typed_value, &AppPackageLimits::default())
                .unwrap_err()
                .to_string()
                .contains("mutation-backed entity-projection")
        );

        let mut false_derived_authority = contribution_port_bundle();
        let skill = false_derived_authority
            .iter_mut()
            .find(|member| member.path.as_str() == "SKILL.md")
            .unwrap();
        skill.bytes = String::from_utf8(skill.bytes.clone())
            .unwrap()
            .replace(
                "evidence_classes: [hypothesis]",
                "evidence_classes: [derived]",
            )
            .into_bytes();
        assert!(
            build_app_package_candidate(false_derived_authority, &AppPackageLimits::default(),)
                .unwrap_err()
                .to_string()
                .contains("must select the hypothesis")
        );
    }

    #[test]
    fn raw_bundle_members_are_move_only_and_admitted_candidates_share_verified_bytes() {
        static_assertions::assert_not_impl_any!(AppBundleMember: Clone);
        static_assertions::assert_impl_all!(AppValidatedBundleMember: Clone);
        static_assertions::assert_impl_all!(AppPackageCandidate: Clone);
        let candidate = build_app_package_candidate(valid_bundle(), &AppPackageLimits::default())
            .expect("validated package");
        let cloned = candidate.clone();
        assert_eq!(candidate.bundle_digest(), cloned.bundle_digest());
        assert_eq!(candidate.members().len(), cloned.members().len());
        for (original, copied) in candidate.members().iter().zip(cloned.members()) {
            assert!(std::sync::Arc::ptr_eq(&original.bytes, &copied.bytes));
        }
    }

    #[test]
    fn canonical_manifest_and_bundle_identity_ignore_input_order() {
        let limits = AppPackageLimits::default();
        let first = build_app_package_candidate(valid_bundle(), &limits).expect("first package");
        let mut reordered = valid_bundle();
        reordered.reverse();
        let second = build_app_package_candidate(reordered, &limits).expect("second package");
        assert_eq!(
            first.manifest().manifest_digest(),
            second.manifest().manifest_digest()
        );
        assert_eq!(first.bundle_digest(), second.bundle_digest());

        let yaml =
            extract_frontmatter(valid_skill_document().as_bytes(), limits.max_manifest_bytes)
                .expect("frontmatter")
                .to_vec();
        let text = String::from_utf8(yaml).expect("utf8");
        let reordered_top = text.replacen(
            "name: learning-plan\nversion: 0.1.0",
            "version: 0.1.0\nname: learning-plan",
            1,
        );
        assert_eq!(
            parse_app_manifest_yaml(text.as_bytes(), &limits)
                .expect("first manifest")
                .manifest_digest(),
            parse_app_manifest_yaml(reordered_top.as_bytes(), &limits)
                .expect("reordered manifest")
                .manifest_digest()
        );
    }

    #[test]
    fn unknown_fields_are_rejected_at_nested_authority_levels() {
        let limits = AppPackageLimits::default();
        for hostile in [
            valid_skill_document().replace(
                "    app_sdk_version: \"1\"",
                "    app_sdk_version: \"1\"\n    invented: true",
            ),
            valid_skill_document().replace(
                "      external_egress: denied",
                "      external_egress: denied\n      grant_everything: true",
            ),
            valid_skill_document().replace(
                "      max_tokens: 100",
                "      max_tokens: 100\n      unlimited: true",
            ),
        ] {
            assert!(matches!(
                parse_app_manifest_frontmatter(hostile.as_bytes(), &limits),
                Err(AppManifestError::InvalidYaml(_))
            ));
        }
    }

    #[test]
    fn required_features_replace_exact_generator_version_authority() {
        let limits = AppPackageLimits::default();
        let generated_by_newer_tool = valid_skill_document().replace(
            "    app_sdk_version: \"1\"",
            "    app_sdk_version: \"999\"\n    required_features:\n      - \
                 typed_entities_v1\n      - governed_actions_v1\n    generated_by:\n      sdk: \
                 magician_app_typescript\n      version: \"99.4.2\"",
        );
        let admitted = parse_app_manifest_frontmatter(generated_by_newer_tool.as_bytes(), &limits)
            .expect("generator version is informational");
        assert_eq!(
            admitted.manifest().metadata.magician.required_features,
            vec![
                AppManifestFeature::TypedEntitiesV1,
                AppManifestFeature::GovernedActionsV1
            ]
        );

        let no_legacy_marker = valid_skill_document().replace("    app_sdk_version: \"1\"\n", "");
        let admitted = parse_app_manifest_frontmatter(no_legacy_marker.as_bytes(), &limits)
            .expect("new manifests need no exact legacy SDK marker");
        assert_eq!(
            admitted.manifest().metadata.magician.app_sdk_version,
            "unknown"
        );

        let unknown_required_feature = valid_skill_document().replace(
            "    app_sdk_version: \"1\"",
            "    app_sdk_version: \"1\"\n    required_features: [teleport_everything_v9]",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(unknown_required_feature.as_bytes(), &limits),
            Err(AppManifestError::InvalidYaml(_))
        ));
    }

    #[test]
    fn aliases_tags_merge_keys_and_block_scalars_are_rejected_before_serde() {
        let limits = AppPackageLimits::default();
        for hostile in [
            b"name: &shared plan\nversion: *shared\n".as_slice(),
            b"name: !custom plan\n".as_slice(),
            b"defaults: &defaults { enabled: true }\ncopy:\n  <<: *defaults\n".as_slice(),
            b"description: |\n  many lines\n".as_slice(),
        ] {
            assert!(matches!(
                parse_app_manifest_yaml(hostile, &limits),
                Err(AppManifestError::YamlAliasOrTagForbidden)
                    | Err(AppManifestError::UnsafeYamlSyntax(_))
            ));
        }
    }

    #[test]
    fn deeply_nested_yaml_rejects_on_a_small_stack() {
        let handle = thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let mut hostile = String::new();
                for depth in 0..256 {
                    hostile.push_str(&"  ".repeat(depth));
                    hostile.push_str("a:\n");
                }
                parse_app_manifest_yaml(hostile.as_bytes(), &AppPackageLimits::default())
            })
            .expect("spawn small-stack parser");
        assert!(matches!(
            handle.join().expect("parser thread did not panic"),
            Err(AppManifestError::YamlDepthLimit { .. })
        ));
    }

    #[test]
    fn compact_block_nesting_rejects_before_serde_on_a_small_stack() {
        let handle = thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let hostile = format!("{}value\n", "- ".repeat(4_096));
                parse_app_manifest_yaml(hostile.as_bytes(), &AppPackageLimits::default())
            })
            .expect("spawn small-stack parser");
        assert!(matches!(
            handle.join().expect("parser thread did not panic"),
            Err(AppManifestError::YamlDepthLimit { .. })
        ));
    }

    #[test]
    fn byte_node_and_string_ceilings_fail_closed() {
        let mut limits = AppPackageLimits::default();
        limits.max_manifest_bytes = 16;
        assert!(matches!(
            parse_app_manifest_frontmatter(valid_skill_document().as_bytes(), &limits),
            Err(AppManifestError::ManifestTooLarge { .. })
        ));

        let mut limits = AppPackageLimits::default();
        limits.max_yaml_nodes = 2;
        assert!(matches!(
            parse_app_manifest_yaml(b"name: app\nversion: 1\nother: 2\n", &limits),
            Err(AppManifestError::YamlNodeLimit { .. })
        ));

        let mut limits = AppPackageLimits::default();
        limits.max_string_bytes = 4;
        assert!(matches!(
            parse_app_manifest_yaml(b"name: abcdef\n", &limits),
            Err(AppManifestError::YamlStringLimit { .. })
                | Err(AppManifestError::InvalidYaml(_))
                | Err(AppManifestError::InvalidManifest(_))
        ));

        let mut limits = AppPackageLimits::default();
        limits.max_string_bytes = 8;
        assert!(matches!(
            parse_app_manifest_yaml(b"name: alpha beta gamma\n", &limits),
            Err(AppManifestError::YamlStringLimit { .. })
        ));

        let mut limits = AppPackageLimits::default();
        limits.max_yaml_nodes = 3;
        assert!(matches!(
            parse_app_manifest_yaml(br#"{"a":1,"b":2}"#, &limits),
            Err(AppManifestError::YamlNodeLimit { .. })
        ));
    }

    #[test]
    fn declarative_surface_components_are_closed_and_bounded() {
        let components = r#"      components:
        - component: section
          id: overview
          children:
            - component: detail
              id: plan_detail
              fields: [topic, status]
            - component: form
              id: create_plan
              fields: [topic, status]
        - component: list
          id: plan_list
          fields: [topic, status]
        - component: table
          id: plan_table
          columns: [topic, status]
"#;
        let valid = valid_skill_document()
            .replace("      route: /\n", &format!("      route: /\n{components}"));
        let parsed = parse_app_manifest_frontmatter(valid.as_bytes(), &AppPackageLimits::default())
            .expect("closed component set should be admitted");
        assert_eq!(
            parsed
                .manifest()
                .app
                .views
                .get(&AppName::parse("plans").unwrap())
                .expect("plans view")
                .components
                .len(),
            3
        );

        let executable_escape = valid.replacen(
            "              fields: [topic, status]",
            "              fields: [topic, status]\n              script: alert(1)",
            1,
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(
                executable_escape.as_bytes(),
                &AppPackageLimits::default(),
            ),
            Err(AppManifestError::InvalidYaml(_))
        ));

        let duplicate = valid.replacen("          id: plan_list", "          id: overview", 1);
        assert!(matches!(
            parse_app_manifest_frontmatter(duplicate.as_bytes(), &AppPackageLimits::default()),
            Err(AppManifestError::InvalidManifest(message))
                if message.contains("repeats surface component id")
        ));

        let missing_field =
            valid.replacen("fields: [topic, status]", "fields: [topic, missing]", 1);
        assert!(matches!(
            parse_app_manifest_frontmatter(missing_field.as_bytes(), &AppPackageLimits::default()),
            Err(AppManifestError::InvalidManifest(message))
                if message.contains("references missing field")
        ));
    }

    #[test]
    fn normalized_duplicate_identifiers_and_routes_are_rejected() {
        let limits = AppPackageLimits::default();
        let duplicate_entity = valid_skill_document().replace(
            "  views:\n",
            "    PLAN:\n      fields:\n        title: { type: text }\n  views:\n",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(duplicate_entity.as_bytes(), &limits),
            Err(AppManifestError::DuplicateNormalizedIdentifier(_))
        ));

        let duplicate_route = valid_skill_document().replace(
            "  workflows:\n",
            "    alternate:\n      entity: plan\n      kind: table\n      route: /\n  workflows:\n",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(duplicate_route.as_bytes(), &limits),
            Err(AppManifestError::DuplicateRoute(_))
        ));

        let static_parameter_overlap = valid_skill_document()
            .replacen("      route: /\n", "      route: /plans/:topic\n", 1)
            .replace(
                "  workflows:\n",
                "    create_plan:\n      entity: plan\n      kind: table\n      route: \
                 /plans/new\n      columns: [title, status]\n  workflows:\n",
            );
        assert!(matches!(
            parse_app_manifest_frontmatter(static_parameter_overlap.as_bytes(), &limits),
            Err(AppManifestError::DuplicateRoute(_))
        ));

        let parameter_alias_overlap = valid_skill_document()
            .replacen("      route: /\n", "      route: /plans/:topic\n", 1)
            .replace(
                "  workflows:\n",
                "    alternate_plan:\n      entity: plan\n      kind: table\n      route: \
                 /plans/:status\n      columns: [title, status]\n  workflows:\n",
            );
        assert!(matches!(
            parse_app_manifest_frontmatter(parameter_alias_overlap.as_bytes(), &limits),
            Err(AppManifestError::DuplicateRoute(_))
        ));

        let capability_alias = valid_skill_document().replace(
            "  actions:\n",
            "    shadow:\n      prompt: workflows/build.md\n      runner: auto\n      uses: \
             [CONTENT_SEARCH]\n      input:\n        type: object\n        fields:\n          \
             topic: { type: text }\n      result:\n        kind: entity_projection\n        \
             entities: [plan]\n      may_mutate: [plan]\n      trigger: user\n  actions:\n",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(capability_alias.as_bytes(), &limits),
            Err(AppManifestError::DuplicateNormalizedIdentifier(_))
        ));
    }

    #[test]
    fn workflow_capabilities_must_be_declared_with_an_exact_requirement() {
        let undeclared = valid_skill_document().replace(
            "    capabilities:\n      - capability: content_search\n        version_requirement: \
             \"^1\"\n",
            "    capabilities: []\n",
        );
        let error =
            parse_app_manifest_frontmatter(undeclared.as_bytes(), &AppPackageLimits::default())
                .expect_err("undeclared workflow capability must fail closed");
        assert!(error.to_string().contains("undeclared tool"));
    }

    #[test]
    fn app_workflows_cannot_declare_owner_facing_governed_tools() {
        let document = valid_skill_document()
            .replace("uses: [content_search]", "uses: [app_data_query]")
            .replace("capability: content_search", "capability: app_data_query");
        let error =
            parse_app_manifest_frontmatter(document.as_bytes(), &AppPackageLimits::default())
                .expect_err("owner-facing governed tools require direct local chat");
        assert!(error
            .to_string()
            .contains("cannot declare owner-facing governed tool"));
    }

    #[test]
    fn tools_field_is_the_canonical_authoring_shape() {
        let modern = valid_skill_document().replace(
            "    capabilities:\n      - capability: content_search\n        version_requirement: \
             \"^1\"\n",
            "    tools:\n      - name: content_search\n        version_requirement: \"^1\"\n",
        );
        let package =
            parse_app_manifest_frontmatter(modern.as_bytes(), &AppPackageLimits::default())
                .expect("tools field is accepted");
        let tools = package.manifest().declared_tools().expect("declared tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name.as_str(), "content_search");
    }

    #[test]
    fn tools_and_capabilities_together_fail_closed() {
        let both = valid_skill_document().replace(
            "    capabilities:\n      - capability: content_search\n        version_requirement: \
             \"^1\"\n",
            "    tools:\n      - name: content_search\n        version_requirement: \"^1\"\n    \
             capabilities:\n      - capability: content_search\n        version_requirement: \
             \"^1\"\n",
        );
        let error = parse_app_manifest_frontmatter(both.as_bytes(), &AppPackageLimits::default())
            .expect_err("both authoring fields fail closed");
        assert!(error.to_string().contains("not both"));
    }

    #[test]
    fn workflow_may_declare_a_named_agent_and_personality() {
        let modern = valid_skill_document().replace(
            "      runner: auto\n      uses: [content_search]\n",
            "      runner: auto\n      agent: research-agent\n      personality: brutal\n      \
             uses: [content_search]\n",
        );
        let package =
            parse_app_manifest_frontmatter(modern.as_bytes(), &AppPackageLimits::default())
                .expect("named runner is accepted");
        let workflow = package
            .manifest()
            .app
            .workflows
            .get(&AppName::parse("build").unwrap())
            .expect("build workflow");
        assert_eq!(
            workflow.agent.as_ref().map(AppName::as_str),
            Some("research-agent")
        );
        assert_eq!(
            workflow.personality.as_ref().map(AppName::as_str),
            Some("brutal")
        );
        assert_eq!(
            package
                .manifest()
                .declared_agents()
                .iter()
                .map(AppName::as_str)
                .collect::<Vec<_>>(),
            vec!["research-agent"]
        );
    }

    #[test]
    fn exact_duplicate_yaml_keys_are_rejected() {
        let duplicate = valid_skill_document().replace(
            "name: learning-plan",
            "name: learning-plan\nname: shadow-plan",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(duplicate.as_bytes(), &AppPackageLimits::default()),
            Err(AppManifestError::InvalidYaml(_))
        ));
    }

    #[test]
    fn board_and_host_global_or_traversing_routes_are_rejected() {
        let limits = AppPackageLimits::default();
        let board = valid_skill_document().replace("kind: list", "kind: board");
        assert!(matches!(
            parse_app_manifest_frontmatter(board.as_bytes(), &limits),
            Err(AppManifestError::UnsupportedViewKind {
                ref view,
                kind: "board"
            }) if view == "plans"
        ));
        for route in [
            "api/apps",
            "/api/apps",
            "/../secret",
            "/_app/chunk",
            "/a//b",
            "/a?x=1",
            "/a/%2e%2e/b",
            "/a/＼b",
            "/ａｐｉ/apps",
        ] {
            let parsed = AppRoute::parse(route);
            assert!(parsed.is_err(), "hostile route accepted: {route}");
        }
    }

    #[test]
    fn installable_v1_app_requires_at_least_one_default_view() {
        let without_views = valid_skill_document().replace(
            "  views:\n    plans:\n      entity: plan\n      kind: list\n      route: /\n",
            "  views: {}\n",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(
                without_views.as_bytes(),
                &AppPackageLimits::default()
            ),
            Err(AppManifestError::InvalidManifest(message))
                if message.contains("views must not be empty")
        ));
    }

    pub fn widget_skill_document() -> String {
        valid_skill_document()
            .replace(
                "    app_sdk_version: \"1\"\n",
                "    app_sdk_version: \"1\"\n    required_features: [app_widgets_v1]\n",
            )
            .replace(
                "  compatibility:\n    magician_contract: \"1\"\n",
                r#"  compatibility:
    magician_contract: "1"
  distribution: system
"#,
            )
            .replace(
                "  assets: [assets/icon.svg]\n",
                r#"  widgets:
    - id: current_plans
      title: Current plans
      view: plans
      read:
        kind: view_projection
        view: plans
        entity: plan
        fields: [topic, status]
      refresh_hint:
        min_interval_seconds: 30
        max_staleness_seconds: 300
      bounds:
        max_rows: 8
        max_render_bytes: 16384
      rendering: { kind: native }
      fallback: { kind: unavailable }
      suggested_slots:
        - page: /
          slot: primary
          system_default: true
      required_capabilities: [declarative_list_v1]
  indicators:
    - id: plan_state
      title: Plan state
      read:
        kind: view_projection
        view: plans
        entity: plan
        fields: [status]
      refresh_hint:
        min_interval_seconds: 60
        max_staleness_seconds: 300
      projection: { kind: state, field: status }
      bounds:
        max_text_bytes: 64
  navigation:
    - id: plans
      title: Plans
      route: /plans
      placement: { kind: section, section: work }
      surface: { kind: view, view: plans }
  assets: [assets/icon.svg]
"#,
            )
    }

    #[test]
    fn app_widgets_v1_admits_only_closed_bounded_view_projections() {
        let manifest = parse_app_manifest_frontmatter(
            widget_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .expect("closed app_widgets_v1 declaration");
        assert_eq!(
            manifest.manifest.app.distribution,
            AppManifestDistribution::System
        );
        assert_eq!(manifest.manifest.app.widgets.len(), 1);
        assert_eq!(manifest.manifest.app.indicators.len(), 1);
        assert_eq!(manifest.manifest.app.navigation.len(), 1);

        let missing_feature =
            widget_skill_document().replace("    required_features: [app_widgets_v1]\n", "");
        assert!(parse_app_manifest_frontmatter(
            missing_feature.as_bytes(),
            &AppPackageLimits::default(),
        )
        .is_err());

        let executable_looking_read = widget_skill_document().replace(
            "        kind: view_projection\n        view: plans\n        entity: plan",
            "        kind: view_projection\n        view: plans\n        binder: internal_data\n        action: list_plans\n        entity: plan",
        );
        assert!(parse_app_manifest_frontmatter(
            executable_looking_read.as_bytes(),
            &AppPackageLimits::default(),
        )
        .is_err());

        // Distribution is an independently provenance-checked class. The
        // source-agnostic parser may describe a system package with no
        // surfacing declarations (for example system ops tooling), while an
        // untrusted publication owner must still reject the class claim.
        let system_without_surfaces = valid_skill_document().replace(
            "  compatibility:\n    magician_contract: \"1\"\n",
            "  compatibility:\n    magician_contract: \"1\"\n  distribution: system\n",
        );
        let parsed = parse_app_manifest_frontmatter(
            system_without_surfaces.as_bytes(),
            &AppPackageLimits::default(),
        )
        .expect("system distribution does not imply app_widgets_v1");
        assert_eq!(
            parsed.manifest.app.distribution,
            AppManifestDistribution::System
        );
    }

    #[test]
    fn absent_distribution_and_widget_blocks_preserve_the_legacy_canonical_shape() {
        let manifest = parse_app_manifest_frontmatter(
            valid_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .expect("legacy additive baseline");
        assert_eq!(
            manifest.manifest.app.distribution,
            AppManifestDistribution::Installable
        );
        let value = serde_json::to_value(manifest.manifest()).expect("canonical manifest value");
        let app = value["app"].as_object().expect("manifest app object");
        for absent in ["distribution", "widgets", "indicators", "navigation"] {
            assert!(
                !app.contains_key(absent),
                "legacy shape injected `{absent}`"
            );
        }
    }

    /// Every shell re-parses a declared route under a grammar tighter than the
    /// host's, and refuses the entire directory page when one entry will not
    /// decode. That makes these two narrowings admission rules rather than
    /// client taste: a package may not claim the site root, and a segment must
    /// stay inside the cap the shells mirror. Both are refusals a seed author
    /// can read; neither is reachable from an installable package, because
    /// navigation is system-distribution only.
    #[test]
    fn first_party_navigation_refuses_routes_no_shell_can_mount() {
        let refusal = |document: String| -> String {
            parse_app_manifest_frontmatter(document.as_bytes(), &AppPackageLimits::default())
                .expect_err("a route no shell can mount is refused")
                .to_string()
        };

        let site_root =
            widget_skill_document().replace("      route: /plans\n", "      route: /\n");
        assert!(refusal(site_root).contains("below the site root"));

        let over_cap = widget_skill_document().replace(
            "      route: /plans\n",
            &format!(
                "      route: /{}\n",
                "p".repeat(APP_NAVIGATION_MAX_ROUTE_SEGMENT_BYTES + 1)
            ),
        );
        assert!(refusal(over_cap).contains("below the site root"));

        // The entry point a declaration names rides along in the same payload
        // and is decoded by the same grammar, so it is held to the same cap.
        let long_entry_point = "e".repeat(APP_NAVIGATION_MAX_ROUTE_SEGMENT_BYTES + 1);
        let custom_surface_nav = widget_skill_document()
            .replace(
                "    required_features: [app_widgets_v1]\n",
                "    required_features: [app_widgets_v1, custom_surfaces_v1]\n",
            )
            .replacen(
                "app:\n",
                &format!(
                    "app:\n  permissions: [custom_surface]\n  custom_surface:\n    \
                     entry_points:\n      - route: /{long_entry_point}\n        \
                     document: surfaces/canvas.html\n"
                ),
                1,
            )
            .replace(
                "      surface: { kind: view, view: plans }\n",
                &format!(
                    "      surface:\n        kind: custom_surface\n        \
                     entry_point: /{long_entry_point}\n        fallback_view: plans\n"
                ),
            );
        assert!(refusal(custom_surface_nav).contains("no shell can mount"));

        // The boundary itself still admits, so each refusal above is this cap
        // and not some unrelated route rule doing the work for it.
        let at_cap = widget_skill_document().replace(
            "      route: /plans\n",
            &format!(
                "      route: /{}\n",
                "p".repeat(APP_NAVIGATION_MAX_ROUTE_SEGMENT_BYTES)
            ),
        );
        parse_app_manifest_frontmatter(at_cap.as_bytes(), &AppPackageLimits::default())
            .expect("a navigation route at the mirrored segment cap stays admissible");
    }

    /// The widget document with an indicator that carries `selector`, plus
    /// the required timestamp an ordered-first selector needs to rank by.
    ///
    /// The unpatched document keeps the pre-contract indicator shape on
    /// purpose: it is what the inertness pins measure against.
    pub fn indicator_selector_document(selector: &str) -> String {
        widget_skill_document()
            .replace(
                "        status: { type: enum, values: [new, done], required: true }\n",
                "        status: { type: enum, values: [new, done], required: true }\n        \
                 updated_at: { type: timestamp, required: true }\n",
            )
            .replace(
                "        fields: [status]\n",
                "        fields: [status, updated_at]\n",
            )
            .replace(
                "      bounds:\n        max_text_bytes: 64\n",
                &format!(
                    "      bounds:\n        max_text_bytes: 64\n      selector:\n{selector}\n"
                ),
            )
    }

    /// The exact-selector contract is a closed vocabulary, checked against the
    /// indicator's own closed read and its entity's declared field types.
    ///
    /// Each refusal here is a way an indicator could otherwise end up
    /// describing a record nobody named: a filter over a field the read never
    /// showed, a value the field can never take, or an order that ties.
    #[test]
    fn indicator_selectors_are_exact_or_refused() {
        for accepted in [
            "        kind: sole_record",
            "        kind: exact_record\n        filter:\n          - field: status\n            equals: done",
            "        kind: ordered_first\n        order:\n          field: updated_at\n          direction: descending",
            "        kind: ordered_first\n        order:\n          field: updated_at\n          direction: ascending\n        filter:\n          - field: status\n            equals: new",
        ] {
            let manifest = parse_app_manifest_frontmatter(
                indicator_selector_document(accepted).as_bytes(),
                &AppPackageLimits::default(),
            )
            .expect("exact indicator selector");
            assert!(manifest.manifest.app.indicators[0].selector.is_some());
        }

        for (refused, reason) in [
            (
                "        kind: exact_record\n        filter: []",
                "must match on at least one field",
            ),
            (
                "        kind: exact_record\n        filter:\n          - field: topic\n            equals: rust",
                "absent from its closed read",
            ),
            (
                "        kind: exact_record\n        filter:\n          - field: status\n            equals: shipped",
                "is not an exact value of field",
            ),
            (
                "        kind: exact_record\n        filter:\n          - field: status\n            equals: done\n          - field: status\n            equals: new",
                "more than once",
            ),
            (
                "        kind: exact_record\n        filter:\n          - field: updated_at\n            equals: \"2026-09-04T00:00:00Z\"",
                "is not an exact value of field",
            ),
            (
                "        kind: ordered_first\n        order:\n          field: status\n          direction: descending",
                "must be a required, non-nullable integer or timestamp",
            ),
            (
                "        kind: ordered_first\n        order:\n          field: topic\n          direction: descending",
                "absent from its closed read",
            ),
        ] {
            let error = parse_app_manifest_frontmatter(
                indicator_selector_document(refused).as_bytes(),
                &AppPackageLimits::default(),
            )
            .expect_err("inexact indicator selector must be refused");
            assert!(
                error.to_string().contains(reason),
                "expected `{reason}` for selector `{refused}`, got `{error}`"
            );
        }
    }

    /// An indicator with no selector still parses, still serializes to the
    /// pre-contract shape, and still says nothing about which record it means.
    #[test]
    fn an_indicator_without_a_selector_keeps_its_pre_contract_shape() {
        let manifest = parse_app_manifest_frontmatter(
            widget_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .expect("pre-contract indicator declaration stays valid");
        assert!(manifest.manifest.app.indicators[0].selector.is_none());
        let value = serde_json::to_value(manifest.manifest()).expect("canonical manifest value");
        let indicator = &value["app"]["indicators"][0];
        assert!(
            indicator.get("selector").is_none(),
            "an absent selector must not be injected into the canonical shape"
        );
    }

    #[test]
    fn widget_fallback_capability_and_system_default_rules_fail_closed() {
        let installable_default = widget_skill_document()
            .replace("  distribution: system\n", "  distribution: installable\n");
        assert!(parse_app_manifest_frontmatter(
            installable_default.as_bytes(),
            &AppPackageLimits::default(),
        )
        .is_err());

        let missing_view_capability = widget_skill_document().replace(
            "      required_capabilities: [declarative_list_v1]\n",
            "      required_capabilities: []\n",
        );
        assert!(parse_app_manifest_frontmatter(
            missing_view_capability.as_bytes(),
            &AppPackageLimits::default(),
        )
        .is_err());

        let unbounded_rows =
            widget_skill_document().replace("        max_rows: 8", "        max_rows: 101");
        assert!(parse_app_manifest_frontmatter(
            unbounded_rows.as_bytes(),
            &AppPackageLimits::default(),
        )
        .is_err());

        let unbound_required_action = widget_skill_document()
            .replace(
                "    required_features: [app_widgets_v1]",
                "    required_features: [app_widgets_v1, governed_actions_v1]",
            )
            .replace(
                "      rendering: { kind: native }",
                "      actions:\n        - id: create\n          label: Create plan\n          governed_action: create\n      rendering: { kind: native }",
            )
            .replace(
                "      required_capabilities: [declarative_list_v1]",
                "      required_capabilities: [declarative_list_v1, governed_actions_v1]",
            );
        let error = parse_app_manifest_frontmatter(
            unbound_required_action.as_bytes(),
            &AppPackageLimits::default(),
        )
        .expect_err("widget action without a closed input binding must fail");
        assert!(error.to_string().contains("declares no input binding"));

        let duplicate_governed_action = widget_skill_document()
            .replace(
                "    required_features: [app_widgets_v1]",
                "    required_features: [app_widgets_v1, governed_actions_v1]",
            )
            .replace(
                "      rendering: { kind: native }",
                "      actions:\n        - id: create_first\n          label: Create first\n          governed_action: create\n        - id: create_second\n          label: Create second\n          governed_action: create\n      rendering: { kind: native }",
            )
            .replace(
                "      required_capabilities: [declarative_list_v1]",
                "      required_capabilities: [declarative_list_v1, governed_actions_v1]",
            )
            // A widget button may only expose an action whose workflow accepts
            // the exact empty object -- v1 has no row-to-action input mapping,
            // and the case just above proves that rule fires. Without relaxing
            // the input here it fires FIRST, and the duplicate-action rule this
            // case exists to exercise is never reached.
            .replace(
                "          topic: { type: text, required: true }",
                "          topic: { type: text }",
            );
        let error = parse_app_manifest_frontmatter(
            duplicate_governed_action.as_bytes(),
            &AppPackageLimits::default(),
        )
        .expect_err("two local widget actions must not collapse to one wire action id");
        assert!(
            error
                .to_string()
                .contains("repeats governed action `create`"),
            "unexpected error: {error}"
        );

        let badge_without_bound = widget_skill_document()
            .replace(
                "status: { type: enum, values: [new, done], required: true }",
                "status: { type: integer, required: true }",
            )
            .replace(
                "projection: { kind: state, field: status }",
                "projection: { kind: badge, field: status }",
            );
        let error = parse_app_manifest_frontmatter(
            badge_without_bound.as_bytes(),
            &AppPackageLimits::default(),
        )
        .expect_err("badge projection must declare its badge-only ceiling");
        assert!(error.to_string().contains("bounds exceed"));

        let bounded_badge = badge_without_bound.replace(
            "        max_text_bytes: 64",
            "        max_text_bytes: 64\n        max_badge_value: 100",
        );
        parse_app_manifest_frontmatter(bounded_badge.as_bytes(), &AppPackageLimits::default())
            .expect("badge projection with an integer field and explicit ceiling is admitted");
    }

    #[test]
    fn route_parameters_require_explicit_portable_scalar_entity_fields() {
        let missing = valid_skill_document().replacen(
            "      route: /\n",
            "      route: /plans/:missing\n",
            1,
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(missing.as_bytes(), &AppPackageLimits::default()),
            Err(AppManifestError::InvalidManifest(message))
                if message.contains("has no matching entity field")
        ));

        let timestamp = valid_skill_document()
            .replace(
                "        status: { type: enum, values: [new, done], required: true }",
                "        status: { type: enum, values: [new, done], required: true }\n        \
                 occurred: { type: timestamp, required: true }",
            )
            .replacen("      route: /\n", "      route: /at/:occurred\n", 1);
        assert!(matches!(
            parse_app_manifest_frontmatter(timestamp.as_bytes(), &AppPackageLimits::default()),
            Err(AppManifestError::InvalidManifest(message))
                if message.contains("portable scalar field")
        ));
    }

    #[test]
    fn timeline_bindings_are_explicit_and_type_checked() {
        let limits = AppPackageLimits::default();
        let timeline = valid_skill_document()
            .replace(
                "        status: { type: enum, values: [new, done], required: true }",
                "        status: { type: enum, values: [new, done], required: true }\n        \
                     occurred: { type: timestamp, required: true }\n        activity: { type: \
                     text, required: true }",
            )
            .replace(
                "  workflows:\n",
                "    history:\n      entity: plan\n      kind: timeline\n      route: \
                     /history\n      timestamp_field: occurred\n      action_field: activity\n  \
                     workflows:\n",
            );
        parse_app_manifest_frontmatter(timeline.as_bytes(), &limits)
            .expect("typed timeline manifest");

        let missing_action = timeline.replace("      action_field: activity\n", "");
        assert!(matches!(
            parse_app_manifest_frontmatter(missing_action.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let wrong_timestamp = timeline.replace("type: timestamp", "type: boolean");
        assert!(matches!(
            parse_app_manifest_frontmatter(wrong_timestamp.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));

        for optional_binding in ["actor_field", "type_field", "target_field"] {
            let hostile = timeline
                .replace(
                    "        activity: { type: text, required: true }",
                    "        activity: { type: text, required: true }\n        visible: { type: \
                     boolean }",
                )
                .replace(
                    "      action_field: activity",
                    &format!("      action_field: activity\n      {optional_binding}: visible"),
                );
            assert!(matches!(
                parse_app_manifest_frontmatter(hostile.as_bytes(), &limits),
                Err(AppManifestError::InvalidManifest(_))
            ));
        }
    }

    #[test]
    fn tree_bindings_require_stack_safe_parent_order_and_partition_shapes() {
        let limits = AppPackageLimits::default();
        let tree = valid_skill_document()
            .replace(
                "        status: { type: enum, values: [new, done], required: true }",
                "        status: { type: enum, values: [new, done], required: true }\n        \
                 parent: { type: reference, entity: plan, nullable: true }\n        position: { \
                 type: integer, required: true }\n        title: { type: text, required: true }",
            )
            .replace(
                "  workflows:\n",
                "    outline:\n      entity: plan\n      kind: tree\n      route: /outline\n      \
                 partition_field: topic\n      parent_field: parent\n      order_field: \
                 position\n      status_field: status\n  workflows:\n",
            );
        parse_app_manifest_frontmatter(tree.as_bytes(), &limits).expect("typed tree manifest");

        let non_nullable = tree.replace(
            "entity: plan, nullable: true",
            "entity: plan, nullable: false",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(non_nullable.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let non_numeric = tree.replace(
            "type: integer, required: true",
            "type: text, required: true",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(non_numeric.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let unstable_partition = tree.replace(
            "topic: { type: text, required: true }",
            "topic: { type: boolean, required: true }",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(unstable_partition.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let missing_label = tree.replace("        title: { type: text, required: true }\n", "");
        assert!(matches!(
            parse_app_manifest_frontmatter(missing_label.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
    }

    #[test]
    fn graph_bindings_require_a_self_reference_parent_and_a_label() {
        let limits = AppPackageLimits::default();
        let graph = valid_skill_document()
            .replace(
                "        status: { type: enum, values: [new, done], required: true }",
                "        status: { type: enum, values: [new, done], required: true }\n        \
                 parent: { type: reference, entity: plan, nullable: true }\n        position: { \
                 type: integer, required: true }",
            )
            .replace(
                "  workflows:\n",
                "    outline:\n      entity: plan\n      kind: graph\n      route: /outline\n      \
                 parent_field: parent\n      order_field: position\n      status_field: status\n  \
                 workflows:\n",
            );
        parse_app_manifest_frontmatter(graph.as_bytes(), &limits).expect("typed graph manifest");

        let missing_parent = graph.replace("      parent_field: parent\n", "");
        assert!(matches!(
            parse_app_manifest_frontmatter(missing_parent.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let non_nullable = graph.replace(
            "entity: plan, nullable: true",
            "entity: plan, nullable: false",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(non_nullable.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let foreign_parent = graph
            .replace(
                "entity: plan, nullable: true",
                "entity: other, nullable: true",
            )
            .replace(
                "  entities:\n    plan:\n",
                "  entities:\n    other:\n      fields:\n        topic: { type: text, required: \
                 true }\n    plan:\n",
            );
        assert!(matches!(
            parse_app_manifest_frontmatter(foreign_parent.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let timeline_binding = graph.replace(
            "      parent_field: parent\n",
            "      parent_field: parent\n      actor_field: topic\n",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(timeline_binding.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let partition_binding = graph.replace(
            "      parent_field: parent\n",
            "      partition_field: topic\n      parent_field: parent\n",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(partition_binding.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let columns = graph.replace(
            "      status_field: status\n",
            "      status_field: status\n      columns: [topic]\n",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(columns.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
        let missing_label = graph.replace(
            "topic: { type: text, required: true }",
            "topic: { type: boolean, required: true }",
        );
        assert!(matches!(
            parse_app_manifest_frontmatter(missing_label.as_bytes(), &limits),
            Err(AppManifestError::InvalidManifest(_))
        ));
    }

    #[test]
    fn reference_integrity_delete_cycle_and_traversal_semantics_are_explicit() {
        let base = valid_skill_document().replace(
            "        status: { type: enum, values: [new, done], required: true }",
            "        status: { type: enum, values: [new, done], required: true }\n        parent: \
             { type: reference, entity: plan, nullable: true }",
        );
        parse_app_manifest_frontmatter(base.as_bytes(), &AppPackageLimits::default())
            .expect("restrict/deny/depth-one defaults are valid");

        let nullify_non_nullable = base.replace(
            "entity: plan, nullable: true",
            "entity: plan, nullable: false, on_delete: nullify",
        );
        assert!(parse_app_manifest_frontmatter(
            nullify_non_nullable.as_bytes(),
            &AppPackageLimits::default(),
        )
        .is_err());

        let cyclic_cascade = base.replace(
            "entity: plan, nullable: true",
            "entity: plan, nullable: true, on_delete: cascade, cycle_policy: allow_bounded",
        );
        assert!(parse_app_manifest_frontmatter(
            cyclic_cascade.as_bytes(),
            &AppPackageLimits::default(),
        )
        .is_err());

        let required_nullable_nullify = base.replace(
            "entity: plan, nullable: true",
            "entity: plan, required: true, nullable: true, on_delete: nullify",
        );
        parse_app_manifest_frontmatter(
            required_nullable_nullify.as_bytes(),
            &AppPackageLimits::default(),
        )
        .expect("required nullable reference can preserve presence by storing null");

        let excessive_depth = base.replace(
            "entity: plan, nullable: true",
            "entity: plan, nullable: true, max_traversal_depth: 17",
        );
        assert!(parse_app_manifest_frontmatter(
            excessive_depth.as_bytes(),
            &AppPackageLimits::default(),
        )
        .is_err());
    }

    #[test]
    fn table_columns_are_explicit_unique_and_bound_to_existing_fields() {
        let limits = AppPackageLimits::default();
        let table = valid_skill_document().replace(
            "      kind: list\n      route: /",
            "      kind: table\n      route: /\n      columns: [topic, status]",
        );
        parse_app_manifest_frontmatter(table.as_bytes(), &limits).expect("typed table");
        for hostile in [
            table.replace("      columns: [topic, status]", "      columns: []"),
            table.replace(
                "      columns: [topic, status]",
                "      columns: [topic, topic]",
            ),
            table.replace(
                "      columns: [topic, status]",
                "      columns: [topic, missing]",
            ),
        ] {
            assert!(matches!(
                parse_app_manifest_frontmatter(hostile.as_bytes(), &limits),
                Err(AppManifestError::InvalidManifest(_))
                    | Err(AppManifestError::DuplicateNormalizedValue { .. })
            ));
        }
    }

    #[test]
    fn unsafe_bundle_paths_links_special_files_and_collisions_are_rejected() {
        for path in [
            "/tmp/file",
            "../file",
            "a/../file",
            "a\\file",
            "a:file",
            "a//file",
            "safe／..／escape",
            "asset：stream",
        ] {
            assert!(
                AppBundlePath::parse(path).is_err(),
                "unsafe path accepted: {path}"
            );
        }

        let limits = AppPackageLimits::default();
        let mut linked = valid_bundle();
        linked.push(AppBundleMember::symlink("escape").expect("link member"));
        assert!(matches!(
            build_app_package_candidate(linked, &limits),
            Err(AppManifestError::SymlinkMember(_))
        ));

        let mut special = valid_bundle();
        special.push(AppBundleMember::special("pipe").expect("special member"));
        assert!(matches!(
            build_app_package_candidate(special, &limits),
            Err(AppManifestError::SpecialMember(_))
        ));

        let mut collision = valid_bundle();
        collision.push(
            AppBundleMember::regular_file("ASSETS/ICON.SVG", b"other".to_vec())
                .expect("collision member"),
        );
        assert!(matches!(
            build_app_package_candidate(collision, &limits),
            Err(AppManifestError::DuplicateNormalizedPath(_))
        ));

        let mut unicode_collision = valid_bundle();
        unicode_collision.push(
            AppBundleMember::regular_file("ａｓｓｅｔｓ/ｉｃｏｎ.svg", b"other".to_vec())
                .expect("compatibility-normalized member"),
        );
        assert!(matches!(
            build_app_package_candidate(unicode_collision, &limits),
            Err(AppManifestError::DuplicateNormalizedPath(_))
        ));
    }

    #[test]
    fn staging_reader_enforces_declared_and_actual_byte_limits_before_admission() {
        struct PanicReader;
        impl Read for PanicReader {
            fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
                panic!("oversized declared member must be rejected before reading")
            }
        }

        let mut admission = AppBundleStagingAdmission::default();
        let oversized = u64::try_from(admission.limits.max_bundle_file_bytes)
            .unwrap()
            .saturating_add(1);
        assert!(matches!(
            admission.push_regular_reader("large.bin", Some(oversized), PanicReader),
            Err(AppManifestError::BundleMemberTooLarge { .. })
        ));

        assert!(matches!(
            admission.push_regular_reader("short.bin", Some(4), &b"abc"[..]),
            Err(AppManifestError::BundleMemberLengthMismatch { .. })
        ));
        admission
            .push_regular_reader("short.bin", Some(3), &b"abc"[..])
            .expect("failed reads do not poison path admission");

        let mut bounded = AppBundleStagingAdmission::default();
        bounded.limits.max_bundle_file_bytes = 4;
        bounded.limits.max_bundle_bytes = 8;
        assert!(matches!(
            bounded.push_regular_reader("unknown.bin", None, &b"abcde"[..]),
            Err(AppManifestError::BundleMemberTooLarge { .. })
        ));
    }

    #[test]
    fn complete_staged_bundle_has_the_same_identity_as_direct_internal_admission() {
        let direct = build_app_package_candidate(valid_bundle(), &AppPackageLimits::default())
            .expect("direct internal admission");
        let mut staged = AppBundleStagingAdmission::default();
        for member in valid_bundle() {
            let byte_len = u64::try_from(member.bytes.len()).unwrap();
            staged
                .push_regular_reader(member.path.as_str(), Some(byte_len), &member.bytes[..])
                .expect("staged member");
        }
        let staged = staged.finish().expect("staged package");
        assert_eq!(direct.bundle_digest(), staged.bundle_digest());
    }

    #[test]
    fn incomplete_bundle_never_produces_a_candidate() {
        let limits = AppPackageLimits::default();
        for missing in [
            "SKILL.md",
            "workflows/build.md",
            "assets/icon.svg",
            "vendor/skills/summarize/SKILL.md",
        ] {
            let bundle = valid_bundle()
                .into_iter()
                .filter(|member| member.path.as_str() != missing)
                .collect();
            assert!(build_app_package_candidate(bundle, &limits).is_err());
        }
    }

    #[test]
    fn every_member_byte_participates_in_bundle_identity() {
        let limits = AppPackageLimits::default();
        let original =
            build_app_package_candidate(valid_bundle(), &limits).expect("original package");
        let mut changed = valid_bundle();
        changed
            .iter_mut()
            .find(|member| member.path.as_str() == "workflows/build.md")
            .expect("workflow")
            .bytes
            .push(b'!');
        let changed = build_app_package_candidate(changed, &limits).expect("changed package");
        assert_ne!(original.bundle_digest(), changed.bundle_digest());
        assert_eq!(
            original.manifest().manifest_digest(),
            changed.manifest().manifest_digest()
        );
    }

    #[test]
    fn frontmatter_and_money_parsing_are_exact() {
        let limits = AppPackageLimits::default();
        let document = valid_skill_document();
        let with_bom = [b"\xef\xbb\xbf".as_slice(), document.as_bytes()].concat();
        assert!(matches!(
            parse_app_manifest_frontmatter(&with_bom, &limits),
            Err(AppManifestError::InvalidFrontmatter(_))
        ));
        assert_eq!(parse_usd_micros("2.500001"), Ok(2_500_001));
        assert!(parse_usd_micros("1e3").is_err());
        assert!(parse_usd_micros("0.0000001").is_err());
        assert!(parse_usd_micros("-1").is_err());
    }

    #[test]
    fn primitive_schema_projection_preserves_nullable_enum_semantics() {
        let schema = AppManifestInputSchema {
            schema_type: AppManifestSchemaType::Object,
            fields: BTreeMap::from([(
                AppName::parse("state").unwrap(),
                AppManifestField::Enum {
                    values: vec![AppName::parse("ready").unwrap()],
                    required: true,
                    nullable: true,
                    data_policy: None,
                },
            )]),
            value_schema: None,
        };
        let projected = schema.to_primitive_json_schema();
        assert_eq!(
            projected.pointer("/properties/state/enum"),
            Some(&serde_json::json!(["ready", null]))
        );
        assert_eq!(
            projected.pointer("/properties/state/type"),
            Some(&serde_json::json!(["string", "null"]))
        );
        schema
            .validate_value(&serde_json::json!({"state": null}))
            .unwrap();
    }

    #[test]
    fn legacy_workflow_schema_serialization_and_identity_remain_unchanged() {
        let schema = AppManifestInputSchema {
            schema_type: AppManifestSchemaType::Object,
            fields: BTreeMap::from([(
                AppName::parse("value").unwrap(),
                AppManifestField::Text {
                    required: true,
                    nullable: false,
                    data_policy: None,
                },
            )]),
            value_schema: None,
        };
        let serialized = serde_json::to_value(&schema).unwrap();
        assert_eq!(serialized.as_object().unwrap().len(), 2);
        assert!(serialized.get("value_schema").is_none());
        assert_eq!(
            AppDigest::blake3_canonical_json(&serialized).unwrap(),
            AppDigest::blake3_canonical_json(&serde_json::json!({
                "type": "object",
                "fields": {
                    "value": { "type": "text", "required": true, "nullable": false }
                }
            }))
            .unwrap()
        );
        let rich = AppManifestInputSchema {
            schema_type: AppManifestSchemaType::Object,
            fields: BTreeMap::new(),
            value_schema: Some(schema.legacy_value_schema_source()),
        };
        assert_eq!(
            schema.value_schema_digest().unwrap(),
            rich.value_schema_digest().unwrap()
        );
    }

    #[test]
    fn canonical_schema_denies_recursion_unknown_union_and_item_overflow() {
        let floor = AppWorkflowHandlingFloor {
            classification: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
        };
        let recursive = AppManifestInputSchema {
            schema_type: AppManifestSchemaType::Object,
            fields: BTreeMap::new(),
            value_schema: Some(AppWorkflowValueSchemaSource {
                version: AppWorkflowValueSchemaVersion::V1,
                root: 0,
                handling_floor: floor.clone(),
                nodes: vec![AppWorkflowValueTypeNode::Nullable { value_type: 0 }],
            }),
        };
        assert!(recursive.compiled_value_schema().is_err());

        let tagged = AppManifestInputSchema {
            schema_type: AppManifestSchemaType::Object,
            fields: BTreeMap::new(),
            value_schema: Some(AppWorkflowValueSchemaSource {
                version: AppWorkflowValueSchemaVersion::V1,
                root: 0,
                handling_floor: floor,
                nodes: vec![
                    AppWorkflowValueTypeNode::Record {
                        fields: BTreeMap::from([(
                            AppName::parse("items").unwrap(),
                            AppWorkflowRecordField {
                                value_type: 1,
                                required: true,
                            },
                        )]),
                    },
                    AppWorkflowValueTypeNode::Array {
                        items: 2,
                        min_items: 1,
                        max_items: 1,
                    },
                    AppWorkflowValueTypeNode::TaggedUnion {
                        discriminator: AppName::parse("kind").unwrap(),
                        variants: BTreeMap::from([(AppName::parse("text").unwrap(), 3)]),
                    },
                    AppWorkflowValueTypeNode::Text { max_bytes: 8 },
                ],
            }),
        };
        assert!(tagged
            .validate_value(&serde_json::json!({
                "items": [{ "kind": "unknown", "value": "x" }]
            }))
            .is_err());
        assert!(tagged
            .validate_value(&serde_json::json!({
                "items": [
                    { "kind": "text", "value": "x" },
                    { "kind": "text", "value": "y" }
                ]
            }))
            .is_err());
        tagged
            .validate_value(&serde_json::json!({
                "items": [{ "kind": "text", "value": "ok" }]
            }))
            .unwrap();
    }

    #[test]
    fn artifact_result_accepts_only_public_opaque_handle_namespace() {
        let schema_ref =
            AppReference::parse(format!("workflow-schema:blake3:{}", "a".repeat(64))).unwrap();
        let schema = AppManifestInputSchema {
            schema_type: AppManifestSchemaType::Object,
            fields: BTreeMap::new(),
            value_schema: Some(AppWorkflowValueSchemaSource {
                version: AppWorkflowValueSchemaVersion::V1,
                root: 0,
                handling_floor: AppWorkflowHandlingFloor {
                    classification: AppDataClassification::Personal,
                    model_processing: AppModelProcessing::LocalOnly,
                },
                nodes: vec![AppWorkflowValueTypeNode::ArtifactRef {
                    value_schema_ref: schema_ref,
                    max_bytes: 1_024,
                    media_types: BTreeSet::from(["application/json".to_owned()]),
                }],
            }),
        };
        schema
            .validate_result_value(&serde_json::json!("artifact-handle:opaque-one"))
            .unwrap();
        assert!(schema
            .validate_result_value(&serde_json::json!("artifact:task_123"))
            .is_err());
        assert!(schema
            .validate_result_value(&serde_json::json!("artifact-handle:execution_123"))
            .is_err());
    }

    fn custom_surface_manifest(extra_entry_points: &str) -> String {
        valid_skill_document()
            .replacen(
                "    app_sdk_version: \"1\"\n",
                "    app_sdk_version: \"1\"\n    required_features: [custom_surfaces_v1]\n",
                1,
            )
            .replacen(
                "app:\n",
                &format!(
                    "app:\n  permissions: [custom_surface]\n  custom_surface:\n    \
                     entry_points:\n{extra_entry_points}"
                ),
                1,
            )
    }

    fn canvas_entry_point() -> &'static str {
        "      - route: /canvas\n        document: surfaces/canvas.html\n"
    }

    fn custom_surface_bundle() -> Vec<AppBundleMember> {
        let mut members = valid_bundle();
        members.retain(|member| member.path.as_str() != "SKILL.md");
        members.push(
            AppBundleMember::regular_file(
                "SKILL.md",
                custom_surface_manifest(canvas_entry_point()).into_bytes(),
            )
            .expect("custom-surface manifest"),
        );
        members.push(
            AppBundleMember::regular_file(
                "surfaces/canvas.html",
                b"<html><body>canvas</body></html>".to_vec(),
            )
            .expect("canvas document"),
        );
        members.push(
            AppBundleMember::regular_file("surfaces/canvas.js", b"console.log('canvas');".to_vec())
                .expect("canvas script"),
        );
        members
    }

    #[test]
    fn custom_surface_declaration_admits_only_the_full_coherent_block() {
        let limits = AppPackageLimits::default();
        let manifest = parse_app_manifest_frontmatter(
            custom_surface_manifest(canvas_entry_point()).as_bytes(),
            &limits,
        )
        .expect("coherent declaration parses");
        assert_eq!(
            manifest.manifest.app.permissions,
            vec![AppManifestPermission::CustomSurface]
        );
        let declaration = manifest
            .manifest
            .app
            .custom_surface
            .as_ref()
            .expect("declaration present");
        assert_eq!(declaration.entry_points.len(), 1);
        assert_eq!(declaration.entry_points[0].route.as_str(), "/canvas");
        assert_eq!(
            declaration.entry_points[0].document.as_str(),
            "surfaces/canvas.html"
        );

        // Permission without the declaration block is refused.
        let bare_permission = valid_skill_document()
            .replacen(
                "    app_sdk_version: \"1\"\n",
                "    app_sdk_version: \"1\"\n    required_features: [custom_surfaces_v1]\n",
                1,
            )
            .replacen("app:\n", "app:\n  permissions: [custom_surface]\n", 1);
        assert!(parse_app_manifest_frontmatter(bare_permission.as_bytes(), &limits).is_err());

        // Declaration without the permission is refused.
        let no_permission = custom_surface_manifest(canvas_entry_point()).replacen(
            "  permissions: [custom_surface]\n",
            "",
            1,
        );
        assert!(parse_app_manifest_frontmatter(no_permission.as_bytes(), &limits).is_err());

        // Either half without the custom_surfaces_v1 feature is refused.
        let no_feature = custom_surface_manifest(canvas_entry_point()).replacen(
            "    required_features: [custom_surfaces_v1]\n",
            "",
            1,
        );
        assert!(parse_app_manifest_frontmatter(no_feature.as_bytes(), &limits).is_err());
    }

    #[test]
    fn custom_surface_entry_points_are_bounded_unique_and_do_not_shadow_views() {
        let limits = AppPackageLimits::default();
        // Entry-point count is capped at the supervised-session count.
        let mut flood = String::new();
        for index in 0..=APP_CUSTOM_SURFACE_MAX_ENTRY_POINTS {
            flood.push_str(&format!(
                "      - route: /canvas-{index}\n        document: surfaces/canvas-{index}.html\n"
            ));
        }
        assert!(parse_app_manifest_frontmatter(
            custom_surface_manifest(&flood).as_bytes(),
            &limits
        )
        .is_err());

        // Duplicate routes and duplicate documents are refused.
        let duplicate_route =
            custom_surface_manifest(&format!("{}{}", canvas_entry_point(), canvas_entry_point()));
        assert!(parse_app_manifest_frontmatter(duplicate_route.as_bytes(), &limits).is_err());

        // A route that overlaps a declared view route is refused.
        let overlapping =
            custom_surface_manifest("      - route: /\n        document: surfaces/canvas.html\n");
        assert!(parse_app_manifest_frontmatter(overlapping.as_bytes(), &limits).is_err());

        // Entry documents must be surfaces/ HTML members.
        for hostile in [
            "      - route: /canvas\n        document: SKILL.md\n",
            "      - route: /canvas\n        document: assets/canvas.html\n",
            "      - route: /canvas\n        document: surfaces/canvas.js\n",
            "      - route: /canvas\n        document: surfaces/../SKILL.md\n",
        ] {
            assert!(
                parse_app_manifest_frontmatter(
                    custom_surface_manifest(hostile).as_bytes(),
                    &limits
                )
                .is_err(),
                "hostile entry accepted: {hostile}"
            );
        }
    }

    #[test]
    fn custom_surface_bundle_members_are_capped_and_wasm_stays_refused() {
        let limits = AppPackageLimits::default();
        let candidate =
            build_app_package_candidate(custom_surface_bundle(), &limits).expect("declared bundle");
        assert!(candidate.manifest().manifest().app.custom_surface.is_some());

        // A declared entry document that is missing from the bundle fails.
        let mut missing_document = custom_surface_bundle();
        missing_document.retain(|member| member.path.as_str() != "surfaces/canvas.html");
        assert!(build_app_package_candidate(missing_document, &limits).is_err());

        // WASM under surfaces/ is refused while the capability is declared.
        let mut wasm = custom_surface_bundle();
        wasm.push(
            AppBundleMember::regular_file("surfaces/canvas.wasm", b"\0asm".to_vec())
                .expect("wasm member"),
        );
        assert!(build_app_package_candidate(wasm, &limits).is_err());

        // Total executable bytes under surfaces/ are capped even though each
        // member is far below the per-file package cap.
        let mut flood = custom_surface_bundle();
        flood.push(
            AppBundleMember::regular_file(
                "surfaces/huge.js",
                vec![b'a'; APP_CUSTOM_SURFACE_MAX_EXECUTABLE_BYTES],
            )
            .expect("flood member"),
        );
        assert!(build_app_package_candidate(flood, &limits).is_err());
    }

    #[test]
    fn absent_custom_surface_declaration_leaves_the_manifest_baseline_unchanged() {
        let limits = AppPackageLimits::default();
        let manifest = parse_app_manifest_frontmatter(valid_skill_document().as_bytes(), &limits)
            .expect("baseline manifest parses");
        assert!(manifest.manifest.app.permissions.is_empty());
        assert!(manifest.manifest.app.custom_surface.is_none());
        // The additive fields are skipped on the wire, so the canonical
        // manifest digest of a pre-1.6 package is byte-for-byte what it was.
        let canonical = serde_json::to_value(&manifest.manifest).unwrap();
        assert!(canonical["app"]
            .as_object()
            .unwrap()
            .get("permissions")
            .is_none());
        assert!(canonical["app"]
            .as_object()
            .unwrap()
            .get("custom_surface")
            .is_none());
        // Undeclared executable surface members stay refused by the resolver
        // kernel; a bundle that ships them without the declaration still
        // stages (it did before 1.6), it just cannot host them.
        let mut undeclared = valid_bundle();
        undeclared.push(
            AppBundleMember::regular_file("surfaces/index.html", b"<html></html>".to_vec())
                .expect("undeclared document"),
        );
        undeclared.push(
            AppBundleMember::regular_file("surfaces/app.js", b"alert(1)".to_vec())
                .expect("undeclared script"),
        );
        assert!(build_app_package_candidate(undeclared, &limits).is_ok());
    }
}
