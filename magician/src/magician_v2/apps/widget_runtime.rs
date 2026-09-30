//! Host-owned native widget rendering and ambient-indicator materialization.
//!
//! This module deliberately starts after manifest admission. A future
//! manifest adapter compiles reviewed declarations into
//! [`CompiledAppWidgetInstallationPlan`]; public requests can name those
//! declarations, but can never provide a query, predicate, field projection,
//! dependency parameter, or tool name. V1 reads replay only canonical,
//! bounded app-entity queries through the existing authenticated owner
//! adapter. This keeps widgets from becoming a workflow-free tool executor.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex as StdMutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    entity_adapter::{AppEntityAdapterError, AppEntityAdapterService},
    lifecycle::AppInstallationStatus,
    manifest::{
        canonical_view_schema_digest, AppManifestDistribution, AppManifestEntity,
        AppManifestIndicator, AppManifestIndicatorFilter, AppManifestIndicatorLiteral,
        AppManifestIndicatorOrderDirection, AppManifestIndicatorOrderKind,
        AppManifestIndicatorProjection, AppManifestViewKind, AppManifestWidget,
        AppManifestWidgetClientCapability, AppManifestWidgetFallback, AppManifestWidgetRendering,
        AppPackageManifest, AppRoute,
    },
    models::{
        AppComparisonOperator, AppContractLimits, AppDigest, AppFieldPath, AppInstallationId,
        AppName, AppOrderDirection, AppPredicate, AppPredicateNode, AppProtocolVersion,
        AppQueryOrder, AppQueryPage, AppQueryRequest, AppRecordId, AppRecordProjection,
        AppReference, AppRevision, ValidateAppContract,
    },
    package_staging::{AppPackageStager, AppPackageStagingError},
    records::{AppInstallation, AppPackageRevision, AppScope},
    registry::{AppRegistryError, AppRegistryService},
    schema_compiler::canonical_entity_schema_digest,
    slot_assignments::{
        AppSlotInventoryError, AppSlotInventoryResolver, AppSlotInventorySnapshot,
        AppSlotPackageAvailability, AppSlotPackageBinding, AppSlotPackageState,
        AppSlotPickerCandidate, AppSlotSuggestion, AppSlotWidgetBinding,
        MAX_APP_SLOT_INVENTORY_PACKAGES,
    },
    system_boot_admission::TrustedSystemInventoryPin,
};

pub const APP_WIDGET_RENDER_SCHEMA_VERSION_V1: u16 = 1;
pub const APP_WIDGET_RENDER_MAX_PAGE_ITEMS: usize = 12;
pub const APP_WIDGET_RENDER_MAX_REQUEST_BYTES: usize = 32 * 1024;
pub const APP_WIDGET_RENDER_MAX_MODEL_BYTES: usize = 64 * 1024;
pub const APP_WIDGET_RENDER_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const APP_WIDGET_QUERY_MAX_ROWS: u32 = 32;
pub const APP_WIDGET_MAX_REGISTERED_PER_SCOPE: usize = 256;
pub const APP_INDICATOR_MAX_REGISTERED_PER_SCOPE: usize = 128;
pub const APP_INDICATOR_MAX_PAGE_ITEMS: usize = 32;
pub const APP_INDICATOR_MAX_EVALUATIONS_PER_TICK: usize = 16;
pub const APP_INDICATOR_MAX_TEXT_BYTES: usize = 256;
/// An ordered-first indicator reads the leader and the runner-up. The second
/// row never reaches a client; it is the evidence that the first one wins.
const APP_INDICATOR_ORDERED_FIRST_ROWS: u32 = 2;
/// Widget-sized, not page-sized. Every client half pins this same ceiling
/// (`appWidgets.ts`, iOS `AppMiniFramePolicy`, Android
/// `AppWidgetMiniFrameDeclaration`) and refuses a taller declaration, so the
/// transport bound and the host bound cannot drift apart.
pub const APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX: u16 = 480;
const APP_WIDGET_MAX_INERT_REGISTRATION_CACHE: usize = 1_024;
const APP_SURFACING_MIN_REFRESH_SECONDS: i64 = 5;
const APP_SURFACING_MAX_REFRESH_SECONDS: i64 = 24 * 60 * 60;
const APP_WIDGET_UNAVAILABLE_RETRY_SECONDS: u32 = 30;
/// Floor on how far ahead of `rendered_at` the batch deadline may sit. The
/// batch deadline is the minimum over its items, and a shared cache entry can
/// expire milliseconds after `now`; every client half refuses a deadline at or
/// before its clock, and web additionally refuses a page whose
/// `refresh_after - rendered_at` is under the 5 s surfacing minimum. Either
/// surfaced as "temporarily unavailable" on unchanged data.
const APP_WIDGET_REFRESH_SLACK_MIN_SECONDS: i64 = APP_SURFACING_MIN_REFRESH_SECONDS;
/// A cached render within `max(floor, refresh / 5)` of expiry is recomputed
/// rather than served, so a served item never carries a near-now deadline.
const APP_WIDGET_REFRESH_SLACK_DIVISOR: i64 = 5;
const APP_WIDGET_MAX_WARNED_FAILURES: usize = 1_024;

/// Closed capability vocabulary clients advertise for native widget models.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppWidgetClientCapability {
    DetailV1,
    ListV1,
    TableV1,
    TimelineV1,
    TreeV1,
    GraphV1,
    GovernedActionsV1,
}

/// The native model family selected by an admitted declaration compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompiledAppWidgetModelKind {
    Detail,
    List,
    Table,
    Timeline,
    Tree,
    Graph,
}

impl CompiledAppWidgetModelKind {
    fn capability(self) -> AppWidgetClientCapability {
        match self {
            Self::Detail => AppWidgetClientCapability::DetailV1,
            Self::List => AppWidgetClientCapability::ListV1,
            Self::Table => AppWidgetClientCapability::TableV1,
            Self::Timeline => AppWidgetClientCapability::TimelineV1,
            Self::Tree => AppWidgetClientCapability::TreeV1,
            Self::Graph => AppWidgetClientCapability::GraphV1,
        }
    }
}

/// Defined native fallback when a client cannot render the declared model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppWidgetUnsupportedFallback {
    Hide,
    Message { title: String, body: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWidgetGovernedActionModel {
    pub action_id: AppName,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct AppWidgetRenderHints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_field: Option<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_field: Option<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_field: Option<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_field: Option<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_field: Option<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_field: Option<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_field: Option<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_field: Option<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_field: Option<AppFieldPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_field: Option<AppFieldPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWidgetSuggestedSlot {
    pub page: AppRoute,
    pub region: AppName,
    pub system_default: bool,
}

/// A widget's declared escalation to a sandboxed, page-bounded mini-frame.
///
/// Transport, not authority. It rides *beside* a complete native model rather
/// than instead of one, so a client that hosts no frames — or refuses this one
/// for its own page budget — still renders the widget's real content. Nothing
/// runs on the strength of a declaration: the frame needs a separately minted
/// host plan, and the owner grant that mint checks over the package's declared
/// custom-surface entry points is not visible to this compiler.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWidgetMiniFrameDeclaration {
    pub entry_point: AppRoute,
    pub max_height_px: u16,
}

/// Whether a declared entry point is one a client will ever ask a plan for.
///
/// A parameterized route has nothing to bind its parameters from inside a
/// widget region, and `/` names the app's whole surface rather than an entry
/// document, so no host could mint a plan for either. Both client halves
/// narrow the declaration exactly this way before they will fetch, and a
/// declaration that fails the check *there* costs the whole render item rather
/// than only the frame — so the narrowing has to happen here, at the producer.
fn mini_frame_entry_point_is_transportable(entry_point: &AppRoute) -> bool {
    entry_point.as_str() != "/" && entry_point.parameter_names().next().is_none()
}

/// Server-only native widget plan. It has no Deserialize implementation and
/// is accepted only as part of a registry-revalidated installation plan.
#[derive(Debug, Clone)]
pub struct CompiledAppWidgetPlan {
    pub widget_id: AppName,
    pub title: String,
    pub declaration_revision: AppDigest,
    pub model_kind: CompiledAppWidgetModelKind,
    pub render_hints: AppWidgetRenderHints,
    pub query: AppQueryRequest,
    pub refresh_seconds: u32,
    pub maximum_model_bytes: usize,
    pub required_client_capabilities: Vec<AppWidgetClientCapability>,
    pub fallback: AppWidgetUnsupportedFallback,
    pub governed_actions: Vec<AppWidgetGovernedActionModel>,
    pub suggested_slots: Vec<AppWidgetSuggestedSlot>,
    /// The reviewed frame escalation this widget declared, when the host can
    /// carry it. `None` for every native widget, and for a declared frame whose
    /// entry point no client would ask a plan for.
    pub mini_frame: Option<AppWidgetMiniFrameDeclaration>,
}

#[derive(Debug, Clone)]
pub enum CompiledAppIndicatorProjection {
    Chip {
        field: AppFieldPath,
        prefix: Option<String>,
    },
    Badge {
        field: AppFieldPath,
    },
    State {
        field: AppFieldPath,
    },
}

/// How a compiled indicator proves which record it is about.
///
/// This is the compiled half of the manifest's exact-selector contract. Both
/// variants are proved from the returned page, not assumed: an indicator the
/// read cannot pin down is hidden, never resolved by store order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompiledAppIndicatorSelection {
    /// Exactly one record. The read is capped at one row, and a continuation
    /// cursor proves a second record matched, which hides the indicator.
    ExactRecord,
    /// The leader under a declared total order. The read is capped at two
    /// rows: the runner-up is never projected to a client and exists only to
    /// prove the leader is not tied with it.
    OrderedFirst {
        order_field: AppFieldPath,
        order_kind: AppManifestIndicatorOrderKind,
    },
}

impl CompiledAppIndicatorSelection {
    /// Rows the read must request for this selection to be provable. It is
    /// both the plan's ceiling and its floor: one row fewer and an
    /// ordered-first leader could not be separated from its runner-up.
    fn read_rows(&self) -> u32 {
        match self {
            Self::ExactRecord => 1,
            Self::OrderedFirst { .. } => APP_INDICATOR_ORDERED_FIRST_ROWS,
        }
    }
}

/// Server-only indicator plan. Indicator queries are cursor-free entity
/// projections bounded by their selection's row count; arbitrary binders are
/// not accepted.
#[derive(Debug, Clone)]
pub struct CompiledAppIndicatorPlan {
    pub indicator_id: AppName,
    pub title: String,
    pub declaration_revision: AppDigest,
    pub query: AppQueryRequest,
    pub selection: CompiledAppIndicatorSelection,
    pub projection: CompiledAppIndicatorProjection,
    pub refresh_seconds: u32,
    pub maximum_text_bytes: u16,
    pub maximum_badge_value: Option<u32>,
}

/// Provenance is provided by the host-side package admission owner, never by
/// a manifest distribution string. This keeps trusted-system status separate
/// from app-authored declaration bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledAppInstallationProvenance {
    kind: CompiledAppInstallationProvenanceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompiledAppInstallationProvenanceKind {
    Installable,
    TrustedSystem { inventory_digest: AppDigest },
}

impl CompiledAppInstallationProvenance {
    pub fn installable() -> Self {
        Self {
            kind: CompiledAppInstallationProvenanceKind::Installable,
        }
    }

    /// Only crate-owned digest-pinned boot admission may mint this proof.
    pub(crate) fn trusted_system(inventory_digest: AppDigest) -> Self {
        Self {
            kind: CompiledAppInstallationProvenanceKind::TrustedSystem { inventory_digest },
        }
    }

    fn is_trusted_system(&self) -> bool {
        matches!(
            &self.kind,
            CompiledAppInstallationProvenanceKind::TrustedSystem { .. }
        )
    }
}

/// Atomic output of the future manifest-to-runtime compiler. Registration
/// reopens the current installation and immutable package revision before any
/// plan becomes visible.
#[derive(Debug, Clone)]
pub struct CompiledAppWidgetInstallationPlan {
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub package_revision_ref: AppReference,
    pub package_content_digest: AppDigest,
    pub provenance: CompiledAppInstallationProvenance,
    pub widgets: Vec<CompiledAppWidgetPlan>,
    pub indicators: Vec<CompiledAppIndicatorPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWidgetRenderTarget {
    pub installation_id: AppInstallationId,
    pub widget_id: AppName,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppWidgetRenderBatchRequest {
    pub schema_version: u16,
    #[serde(default)]
    pub client_capabilities: Vec<AppWidgetClientCapability>,
    pub widgets: Vec<AppWidgetRenderTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppWidgetRenderRow {
    pub entity: AppName,
    pub record_id: AppRecordId,
    pub record_revision: AppRevision,
    pub fields: BTreeMap<AppFieldPath, Value>,
}

/// Platform-neutral, schema-versioned native render model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "model", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppWidgetNativeRenderModel {
    Detail {
        row: Option<AppWidgetRenderRow>,
        hints: AppWidgetRenderHints,
        actions: Vec<AppWidgetGovernedActionModel>,
    },
    List {
        rows: Vec<AppWidgetRenderRow>,
        hints: AppWidgetRenderHints,
        actions: Vec<AppWidgetGovernedActionModel>,
    },
    Table {
        columns: Vec<AppFieldPath>,
        rows: Vec<AppWidgetRenderRow>,
        hints: AppWidgetRenderHints,
        actions: Vec<AppWidgetGovernedActionModel>,
    },
    Timeline {
        rows: Vec<AppWidgetRenderRow>,
        hints: AppWidgetRenderHints,
        actions: Vec<AppWidgetGovernedActionModel>,
    },
    Tree {
        rows: Vec<AppWidgetRenderRow>,
        hints: AppWidgetRenderHints,
        actions: Vec<AppWidgetGovernedActionModel>,
    },
    Graph {
        rows: Vec<AppWidgetRenderRow>,
        hints: AppWidgetRenderHints,
        actions: Vec<AppWidgetGovernedActionModel>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppWidgetRenderState {
    Ready {
        model: AppWidgetNativeRenderModel,
        /// Present only beside a complete model, because the escalation is
        /// additive: a client asked to run app code in place of content the
        /// host could not produce is the exact inversion the gate refuses.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mini_frame: Option<AppWidgetMiniFrameDeclaration>,
    },
    Unsupported {
        fallback: AppWidgetUnsupportedFallback,
    },
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppWidgetRenderItem {
    pub installation_id: AppInstallationId,
    pub widget_id: AppName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub installation_generation: Option<u64>,
    pub revision: AppDigest,
    pub rendered_at: DateTime<Utc>,
    pub refresh_after: DateTime<Utc>,
    #[serde(flatten)]
    pub render: AppWidgetRenderState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppWidgetRenderBatchResponse {
    pub schema_version: u16,
    pub revision: AppDigest,
    pub etag: String,
    pub rendered_at: DateTime<Utc>,
    pub refresh_after: DateTime<Utc>,
    pub widgets: Vec<AppWidgetRenderItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppWidgetRenderBatchOutcome {
    Modified(AppWidgetRenderBatchResponse),
    NotModified {
        etag: String,
        refresh_after: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppIndicatorRenderModel {
    Chip { text: String },
    Badge { count: u16 },
    State { label: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMaterializedIndicator {
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub indicator_id: AppName,
    pub title: String,
    pub revision: AppDigest,
    pub evaluated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub model: AppIndicatorRenderModel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppIndicatorListResponse {
    pub schema_version: u16,
    pub revision: AppDigest,
    pub etag: String,
    pub generated_at: DateTime<Utc>,
    pub indicators: Vec<AppMaterializedIndicator>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppIndicatorListOutcome {
    Modified(AppIndicatorListResponse),
    NotModified { etag: String },
}

/// Bounded enabled-widget inventory for picker/slot owners. Package identity
/// comes from registry-revalidated immutable metadata; trusted-system status
/// comes from host provenance, never from the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppEnabledWidgetInventoryItem {
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub package_revision_ref: AppReference,
    pub package_id: AppReference,
    pub package_content_digest: AppDigest,
    pub trusted_system: bool,
    pub widget_id: AppName,
    pub title: String,
    pub suggested_slots: Vec<AppWidgetSuggestedSlot>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppIndicatorEvaluationReport {
    pub attempted: usize,
    pub materialized: usize,
    pub hidden: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct InstallationKey {
    scope: AppScope,
    installation_id: AppInstallationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DeclarationKey {
    scope: AppScope,
    installation_id: AppInstallationId,
    generation: u64,
    declaration_id: AppName,
}

#[derive(Debug, Clone)]
struct RegisteredInstallation {
    generation: u64,
    package_revision_ref: AppReference,
    package: AppPackageRevision,
    provenance: CompiledAppInstallationProvenance,
}

#[derive(Debug, Clone)]
struct CachedWidget {
    item: AppWidgetRenderItem,
    fresh_until: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct TargetInstallationSnapshot {
    installation: Option<AppInstallation>,
    /// Captured before the registry read. A lifecycle hook that races the read
    /// or subsequent lazy admission changes this epoch, invalidating the
    /// apparently enabled snapshot before any widget state may be published.
    scope_epoch: u64,
}

#[derive(Default)]
struct RuntimeState {
    installations: HashMap<InstallationKey, RegisteredInstallation>,
    inert_installations: HashMap<InstallationKey, (u64, AppReference)>,
    widgets: HashMap<DeclarationKey, CompiledAppWidgetPlan>,
    indicators: HashMap<DeclarationKey, CompiledAppIndicatorPlan>,
    widget_cache: HashMap<DeclarationKey, CachedWidget>,
    materialized_indicators: HashMap<DeclarationKey, AppMaterializedIndicator>,
    due_by_scope: HashMap<AppScope, BTreeMap<(i64, u64), DeclarationKey>>,
    due_index: HashMap<DeclarationKey, (i64, u64)>,
    next_due_sequence: u64,
    /// Monotonic in-process fence for lifecycle/grant eviction. Reads capture
    /// the scope epoch before awaiting entity I/O and may publish only if it is
    /// unchanged; otherwise an in-flight pre-disable projection could recreate
    /// a cache entry after the lifecycle hook removed it.
    scope_epochs: HashMap<AppScope, u64>,
    /// What the deployment's own boot admission vouched for this process.
    /// `None` until it runs, and `None` is the fail-closed answer: no system
    /// manifest compiles without it. Deployment-wide rather than per scope,
    /// because one seed root produces one inventory.
    trusted_system_inventory: Option<TrustedSystemInventoryPin>,
    /// Failure signatures already logged at warn. Registration, owner-query
    /// and projection failures recur on every client poll; one warn per
    /// installation generation (and reason) keeps them visible without
    /// flooding. Bounded: cleared wholesale when full.
    warned_failures: HashSet<String>,
}

#[derive(Clone)]
pub struct AppWidgetRuntime {
    registry: AppRegistryService,
    package_stager: Option<AppPackageStager>,
    entity_adapter: AppEntityAdapterService,
    state: Arc<StdMutex<RuntimeState>>,
}

impl AppWidgetRuntime {
    pub fn new(registry: AppRegistryService) -> Self {
        Self {
            entity_adapter: AppEntityAdapterService::new(registry.clone()),
            registry,
            package_stager: None,
            state: Arc::new(StdMutex::new(RuntimeState::default())),
        }
    }

    /// Construct the production runtime with the exact content-addressed
    /// package source needed for lazy declaration admission. Test/foundation
    /// owners can keep using [`Self::new`] and register server-owned plans
    /// explicitly.
    pub fn with_package_stager(
        registry: AppRegistryService,
        package_stager: AppPackageStager,
    ) -> Self {
        Self {
            entity_adapter: AppEntityAdapterService::new(registry.clone()),
            registry,
            package_stager: Some(package_stager),
            state: Arc::new(StdMutex::new(RuntimeState::default())),
        }
    }

    /// Adopt this deployment's boot-admitted system-package inventory.
    ///
    /// Called by the host's boot admission, which is the only producer of a
    /// [`TrustedSystemInventoryPin`]. Until it is called, every
    /// `distribution: system` manifest refuses to compile — that refusal is the
    /// fail-closed default rather than a defect, and it is why boot admission
    /// is awaited before the first widget read rather than spawned.
    pub fn adopt_trusted_system_inventory(&self, inventory: &TrustedSystemInventoryPin) {
        let mut state = lock_state(&self.state);
        if state
            .trusted_system_inventory
            .as_ref()
            .is_some_and(|current| current.inventory_digest() == inventory.inventory_digest())
        {
            // Boot admission runs once per scope over a single seed root, so
            // the same pin arrives several times. Re-adopting it would drop the
            // negative-admission cache for nothing.
            return;
        }
        // Any compilation attempted before the pin arrived cached a negative
        // admission for every system package ("a system manifest requires
        // digest-pinned host provenance"), keyed by installation generation and
        // package revision — neither of which changes when the pin lands. Left
        // in place, that cache would keep the deployment's own packages inert
        // for the life of the process.
        state.inert_installations.clear();
        state.trusted_system_inventory = Some(inventory.clone());
    }

    /// Register one exact, already-compiled installation plan. Durable
    /// installation/package identity is reopened before an atomic replacement.
    pub async fn register_compiled_installation(
        &self,
        authenticated: &AuthenticatedAppScope,
        plan: CompiledAppWidgetInstallationPlan,
        now: DateTime<Utc>,
    ) -> Result<(), AppWidgetRuntimeError> {
        validate_installation_plan(&plan)?;
        let admission_epoch = {
            let state = lock_state(&self.state);
            state
                .scope_epochs
                .get(authenticated.scope())
                .copied()
                .unwrap_or(0)
        };
        let installation = self
            .registry
            .installation(authenticated, &plan.installation_id, now)
            .await?
            .ok_or(AppWidgetRuntimeError::MissingInstallation)?;
        validate_registration_installation(&installation, &plan)?;
        let package = self
            .registry
            .package_revision(authenticated, &plan.package_revision_ref, now)
            .await?
            .ok_or(AppWidgetRuntimeError::MissingPackageRevision)?;
        if package.content_digest != plan.package_content_digest {
            return Err(AppWidgetRuntimeError::StalePackageBinding);
        }

        let scope = authenticated.scope().clone();
        let installation_key = InstallationKey {
            scope: scope.clone(),
            installation_id: plan.installation_id.clone(),
        };
        let mut state = lock_state(&self.state);
        if state.scope_epochs.get(&scope).copied().unwrap_or(0) != admission_epoch {
            return Err(AppWidgetRuntimeError::StaleInstallationBinding);
        }
        if state
            .installations
            .get(&installation_key)
            .is_some_and(|current| {
                registration_blocks_candidate(
                    current.generation,
                    &current.package_revision_ref,
                    plan.installation_generation,
                    &plan.package_revision_ref,
                )
            })
        {
            return Err(AppWidgetRuntimeError::StaleInstallationBinding);
        }
        let retained_widgets = state
            .widgets
            .keys()
            .filter(|key| key.scope == scope && key.installation_id != plan.installation_id)
            .count();
        let retained_indicators = state
            .indicators
            .keys()
            .filter(|key| key.scope == scope && key.installation_id != plan.installation_id)
            .count();
        let retained_installations = state
            .installations
            .keys()
            .filter(|key| key.scope == scope && key.installation_id != plan.installation_id)
            .count();
        if retained_widgets.saturating_add(plan.widgets.len()) > APP_WIDGET_MAX_REGISTERED_PER_SCOPE
            || retained_indicators.saturating_add(plan.indicators.len())
                > APP_INDICATOR_MAX_REGISTERED_PER_SCOPE
            || retained_installations.saturating_add(1) > MAX_APP_SLOT_INVENTORY_PACKAGES
        {
            return Err(AppWidgetRuntimeError::ScopePlanCapacityExceeded);
        }

        remove_installation_state(&mut state, &installation_key, true);
        state.installations.insert(
            installation_key,
            RegisteredInstallation {
                generation: plan.installation_generation,
                package_revision_ref: plan.package_revision_ref,
                package,
                provenance: plan.provenance,
            },
        );
        for widget in plan.widgets {
            let key = DeclarationKey {
                scope: scope.clone(),
                installation_id: plan.installation_id.clone(),
                generation: plan.installation_generation,
                declaration_id: widget.widget_id.clone(),
            };
            state.widgets.insert(key, widget);
        }
        for indicator in plan.indicators {
            let key = DeclarationKey {
                scope: scope.clone(),
                installation_id: plan.installation_id.clone(),
                generation: plan.installation_generation,
                declaration_id: indicator.indicator_id.clone(),
            };
            let jitter = evaluator_jitter_seconds(&key, indicator.refresh_seconds);
            state.indicators.insert(key.clone(), indicator);
            schedule_indicator(&mut state, key, now + Duration::seconds(jitter));
        }
        Ok(())
    }

    /// Synchronous invalidation seam used by the registry lifecycle hide hook.
    /// Compiled declarations stay inertly registered, while every derived value
    /// disappears immediately; stale generations can no longer key-hit.
    pub fn evict_scope_materializations(&self, principal: &str, workspace: &str) {
        let mut state = lock_state(&self.state);
        match (
            AppReference::parse(principal.to_owned()),
            AppReference::parse(workspace.to_owned()),
        ) {
            (Ok(principal), Ok(workspace)) => {
                // Fence the exact scope even when its first registration is
                // still in flight and no installation map entry exists yet.
                let epoch = state
                    .scope_epochs
                    .entry(AppScope {
                        principal,
                        workspace,
                    })
                    .or_default();
                *epoch = epoch.wrapping_add(1).max(1);
            },
            _ => {
                // Registry scope tokens are contract-validated, so this is a
                // corruption boundary. Fail closed for every extant scope.
                for epoch in state.scope_epochs.values_mut() {
                    *epoch = epoch.wrapping_add(1).max(1);
                }
                state.widget_cache.clear();
                state.materialized_indicators.clear();
                return;
            },
        }
        state.widget_cache.retain(|key, _| {
            key.scope.principal.as_str() != principal || key.scope.workspace.as_str() != workspace
        });
        state.materialized_indicators.retain(|key, _| {
            key.scope.principal.as_str() != principal || key.scope.workspace.as_str() != workspace
        });
    }

    pub async fn render_batch(
        &self,
        authenticated: &AuthenticatedAppScope,
        request: AppWidgetRenderBatchRequest,
        if_none_match: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<AppWidgetRenderBatchOutcome, AppWidgetRuntimeError> {
        validate_render_request(&request)?;
        let installations = self
            .target_installations_and_ensure_registrations(authenticated, &request.widgets, now)
            .await?;
        let capabilities = request
            .client_capabilities
            .iter()
            .copied()
            .collect::<HashSet<_>>();

        let mut items = Vec::with_capacity(request.widgets.len());
        for target in request.widgets {
            let Some(target_snapshot) = installations.get(&target.installation_id) else {
                items.push(unavailable_widget_item(
                    target,
                    None,
                    now,
                    APP_WIDGET_UNAVAILABLE_RETRY_SECONDS,
                )?);
                continue;
            };
            let Some(installation) = target_snapshot
                .installation
                .as_ref()
                .filter(|installation| {
                    installation.lifecycle.status == AppInstallationStatus::Enabled
                })
            else {
                items.push(unavailable_widget_item(
                    target,
                    None,
                    now,
                    APP_WIDGET_UNAVAILABLE_RETRY_SECONDS,
                )?);
                continue;
            };
            let key = DeclarationKey {
                scope: authenticated.scope().clone(),
                installation_id: target.installation_id.clone(),
                generation: installation.lifecycle.generation,
                declaration_id: target.widget_id.clone(),
            };
            let (registered, plan, cached, read_epoch) = {
                let state = lock_state(&self.state);
                let installation_key = InstallationKey {
                    scope: authenticated.scope().clone(),
                    installation_id: target.installation_id.clone(),
                };
                (
                    state.installations.get(&installation_key).cloned(),
                    state.widgets.get(&key).cloned(),
                    state.widget_cache.get(&key).cloned(),
                    state
                        .scope_epochs
                        .get(authenticated.scope())
                        .copied()
                        .unwrap_or(0),
                )
            };
            if read_epoch != target_snapshot.scope_epoch {
                items.push(unavailable_widget_item(
                    target,
                    Some(installation.lifecycle.generation),
                    now,
                    APP_WIDGET_UNAVAILABLE_RETRY_SECONDS,
                )?);
                continue;
            }
            let Some((_, plan)) = registered
                .filter(|registered| {
                    registered.generation == installation.lifecycle.generation
                        && registered.package_revision_ref == installation.package_revision_ref
                })
                .zip(plan)
            else {
                items.push(unavailable_widget_item(
                    target,
                    Some(installation.lifecycle.generation),
                    now,
                    APP_WIDGET_UNAVAILABLE_RETRY_SECONDS,
                )?);
                continue;
            };

            if !capabilities.contains(&plan.model_kind.capability())
                || (!plan.governed_actions.is_empty()
                    && !capabilities.contains(&AppWidgetClientCapability::GovernedActionsV1))
                || plan
                    .required_client_capabilities
                    .iter()
                    .any(|required| !capabilities.contains(required))
            {
                let plan_is_current = {
                    let state = lock_state(&self.state);
                    widget_plan_is_current(
                        &state,
                        authenticated.scope(),
                        &key,
                        installation,
                        &plan,
                        read_epoch,
                    )
                };
                items.push(if plan_is_current {
                    widget_item(
                        &key,
                        &plan.title,
                        &plan.declaration_revision,
                        now,
                        plan.refresh_seconds,
                        AppWidgetRenderState::Unsupported {
                            fallback: plan.fallback,
                        },
                    )?
                } else {
                    unavailable_widget_item(
                        target,
                        Some(installation.lifecycle.generation),
                        now,
                        plan.refresh_seconds,
                    )?
                });
                continue;
            }
            if let Some(cached) = cached.filter(|cached| {
                cached_widget_is_servable(cached.fresh_until, now, plan.refresh_seconds)
            }) {
                let cache_is_current = {
                    let state = lock_state(&self.state);
                    widget_plan_is_current(
                        &state,
                        authenticated.scope(),
                        &key,
                        installation,
                        &plan,
                        read_epoch,
                    )
                };
                if cache_is_current {
                    items.push(cached.item);
                } else {
                    items.push(unavailable_widget_item(
                        target,
                        Some(installation.lifecycle.generation),
                        now,
                        plan.refresh_seconds,
                    )?);
                }
                continue;
            }

            let mut item = match self
                .entity_adapter
                .owner_query(authenticated, plan.query.clone(), now)
                .await
            {
                Ok(page) => match project_widget_model(&plan, page) {
                    Ok(model) => widget_item(
                        &key,
                        &plan.title,
                        &plan.declaration_revision,
                        now,
                        plan.refresh_seconds,
                        AppWidgetRenderState::Ready {
                            model,
                            mini_frame: plan.mini_frame.clone(),
                        },
                    )?,
                    Err(error) => {
                        self.warn_render_failure(&key, "projection", &error);
                        unavailable_widget_item(
                            target.clone(),
                            Some(installation.lifecycle.generation),
                            now,
                            plan.refresh_seconds,
                        )?
                    },
                },
                Err(error) => {
                    self.warn_render_failure(&key, "owner_query", &error);
                    unavailable_widget_item(
                        target.clone(),
                        Some(installation.lifecycle.generation),
                        now,
                        plan.refresh_seconds,
                    )?
                },
            };
            let mut state = lock_state(&self.state);
            let plan_is_current = widget_plan_is_current(
                &state,
                authenticated.scope(),
                &key,
                installation,
                &plan,
                read_epoch,
            );
            if !plan_is_current {
                state.widget_cache.remove(&key);
                item = unavailable_widget_item(
                    target,
                    Some(installation.lifecycle.generation),
                    now,
                    plan.refresh_seconds,
                )?;
            } else if matches!(&item.render, AppWidgetRenderState::Ready { .. }) {
                state.widget_cache.insert(
                    key.clone(),
                    CachedWidget {
                        item: item.clone(),
                        fresh_until: now + Duration::seconds(i64::from(plan.refresh_seconds)),
                    },
                );
            } else {
                state.widget_cache.remove(&key);
            }
            items.push(item);
        }

        let response = widget_batch_response(items, now)?;
        if response_size(&response)? > APP_WIDGET_RENDER_MAX_RESPONSE_BYTES {
            return Err(AppWidgetRuntimeError::ResponseTooLarge);
        }
        if etag_matches(if_none_match, &response.etag) {
            Ok(AppWidgetRenderBatchOutcome::NotModified {
                etag: response.etag,
                refresh_after: response.refresh_after,
            })
        } else {
            Ok(AppWidgetRenderBatchOutcome::Modified(response))
        }
    }

    /// True the first time this failure signature is seen (bounded memory).
    fn first_warning(&self, signature: String) -> bool {
        let mut state = lock_state(&self.state);
        if state.warned_failures.contains(&signature) {
            return false;
        }
        if state.warned_failures.len() >= APP_WIDGET_MAX_WARNED_FAILURES {
            state.warned_failures.clear();
        }
        state.warned_failures.insert(signature)
    }

    fn warn_render_failure(
        &self,
        key: &DeclarationKey,
        stage: &'static str,
        error: &dyn std::fmt::Display,
    ) {
        let reason = error.to_string();
        let signature = format!(
            "{stage}:{}:{}:{}:{}:{reason}",
            key.scope.workspace.as_str(),
            key.installation_id,
            key.generation,
            key.declaration_id
        );
        if self.first_warning(signature) {
            tracing::warn!(installation_id = %key.installation_id,
                generation = key.generation,
                widget_id = %key.declaration_id,
                stage,
                error = %reason,
                "app widget render failed; serving unavailable");
        } else {
            tracing::debug!(installation_id = %key.installation_id,
                widget_id = %key.declaration_id, stage, error = %reason,
                "app widget render still failing");
        }
    }

    fn registration_is_current(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation: &AppInstallation,
    ) -> bool {
        let state = lock_state(&self.state);
        state
            .installations
            .get(&InstallationKey {
                scope: authenticated.scope().clone(),
                installation_id: installation.installation_id.clone(),
            })
            .is_some_and(|registered| {
                registered.generation == installation.lifecycle.generation
                    && registered.package_revision_ref == installation.package_revision_ref
            })
    }

    fn registration_or_inert_result_is_current(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation: &AppInstallation,
    ) -> bool {
        if self.registration_is_current(authenticated, installation) {
            return true;
        }
        let state = lock_state(&self.state);
        state
            .inert_installations
            .get(&InstallationKey {
                scope: authenticated.scope().clone(),
                installation_id: installation.installation_id.clone(),
            })
            .is_some_and(|(generation, package_revision_ref)| {
                *generation == installation.lifecycle.generation
                    && package_revision_ref == &installation.package_revision_ref
            })
    }

    /// Reopen each distinct target installation once and reuse that same
    /// lifecycle snapshot for lazy declaration admission and rendering. The
    /// scope epoch still fences any lifecycle transition after this read, so a
    /// second per-installation registry round trip adds cost without authority.
    async fn target_installations_and_ensure_registrations(
        &self,
        authenticated: &AuthenticatedAppScope,
        targets: &[AppWidgetRenderTarget],
        now: DateTime<Utc>,
    ) -> Result<HashMap<AppInstallationId, TargetInstallationSnapshot>, AppWidgetRuntimeError> {
        let mut installation_ids = targets
            .iter()
            .map(|target| target.installation_id.clone())
            .collect::<Vec<_>>();
        installation_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        installation_ids.dedup();
        let scope_epoch = lock_state(&self.state)
            .scope_epochs
            .get(authenticated.scope())
            .copied()
            .unwrap_or(0);
        let loaded = self
            .registry
            .installations_by_ids(authenticated, &installation_ids, now)
            .await?;
        self.ensure_installations_registered(authenticated, &loaded, now)
            .await;
        let mut loaded = loaded
            .into_iter()
            .map(|installation| (installation.installation_id.clone(), installation))
            .collect::<HashMap<_, _>>();
        Ok(installation_ids
            .into_iter()
            .map(|id| {
                let installation = loaded.remove(&id);
                (
                    id,
                    TargetInstallationSnapshot {
                        installation,
                        scope_epoch,
                    },
                )
            })
            .collect())
    }

    async fn ensure_installations_registered(
        &self,
        authenticated: &AuthenticatedAppScope,
        installations: &[AppInstallation],
        now: DateTime<Utc>,
    ) {
        if self.package_stager.is_none() {
            return;
        }
        stream::iter(installations.iter().filter(|installation| {
            installation.lifecycle.status == AppInstallationStatus::Enabled
        }))
        .for_each_concurrent(4, |installation| async move {
            if let Err(error) = self
                .ensure_installation_registered(authenticated, installation.clone(), now)
                .await
            {
                let signature = format!(
                    "registration:{}:{}:{}:{}",
                    authenticated.scope().workspace.as_str(),
                    installation.installation_id,
                    installation.lifecycle.generation,
                    error
                );
                if self.first_warning(signature) {
                    tracing::warn!(installation_id = %installation.installation_id,
                        generation = installation.lifecycle.generation,
                        package_revision_ref = %installation.package_revision_ref,
                        error = %error,
                        "app widget declaration registration failed; widget stays unavailable");
                } else {
                    tracing::debug!(installation_id = %installation.installation_id, error = %error,
                        "app widget declaration remains unavailable");
                }
            }
        })
        .await;
    }

    /// Refresh the bounded current-generation declaration inventory for one
    /// authenticated scope. The projection worker calls this on a coarse
    /// cadence; slot reads may also call it before resolving assignments.
    pub async fn refresh_scope_registrations(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<(), AppWidgetRuntimeError> {
        self.reconcile_scope_registrations(authenticated, now)
            .await
            .map(|_| ())
    }

    /// Return the exact enabled snapshot used for reconciliation. Slot
    /// inventory consumes this same snapshot, avoiding a second per-package
    /// registry reopen after the bounded scoped scan.
    async fn reconcile_scope_registrations(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<(Vec<AppInstallation>, u64), AppWidgetRuntimeError> {
        if self.package_stager.is_none() {
            let epoch = lock_state(&self.state)
                .scope_epochs
                .get(authenticated.scope())
                .copied()
                .unwrap_or(0);
            return Ok((Vec::new(), epoch));
        }
        let scan_epoch = {
            let state = lock_state(&self.state);
            state
                .scope_epochs
                .get(authenticated.scope())
                .copied()
                .unwrap_or(0)
        };
        let installations = self
            .registry
            .enabled_installations_bounded(authenticated, MAX_APP_SLOT_INVENTORY_PACKAGES, now)
            .await?;
        if installations.len() > MAX_APP_SLOT_INVENTORY_PACKAGES {
            return Err(AppWidgetRuntimeError::ScopePlanCapacityExceeded);
        }
        let enabled = installations
            .iter()
            .map(|installation| {
                (
                    installation.installation_id.clone(),
                    (
                        installation.lifecycle.generation,
                        installation.package_revision_ref.clone(),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        self.ensure_installations_registered(authenticated, &installations, now)
            .await;

        let stale = {
            let state = lock_state(&self.state);
            if state
                .scope_epochs
                .get(authenticated.scope())
                .copied()
                .unwrap_or(0)
                != scan_epoch
            {
                return Err(AppWidgetRuntimeError::StaleInstallationBinding);
            }
            state
                .installations
                .iter()
                .filter(|(key, registered)| {
                    key.scope == *authenticated.scope()
                        && enabled.get(&key.installation_id).is_none_or(
                            |(generation, package_revision_ref)| {
                                registered.generation != *generation
                                    || registered.package_revision_ref != *package_revision_ref
                            },
                        )
                })
                .map(|(key, registered)| {
                    (
                        key.clone(),
                        registered.generation,
                        registered.package_revision_ref.clone(),
                    )
                })
                .chain(
                    state
                        .inert_installations
                        .iter()
                        .filter(|(key, (generation, package_revision_ref))| {
                            key.scope == *authenticated.scope()
                                && enabled.get(&key.installation_id).is_none_or(
                                    |(current_generation, current_package_revision_ref)| {
                                        generation != current_generation
                                            || package_revision_ref != current_package_revision_ref
                                    },
                                )
                        })
                        .map(|(key, (generation, package_revision_ref))| {
                            (key.clone(), *generation, package_revision_ref.clone())
                        }),
                )
                .collect::<HashSet<_>>()
        };
        for (key, expected_generation, expected_package_revision_ref) in stale {
            let live = self
                .registry
                .installation(authenticated, &key.installation_id, now)
                .await?;
            let mut state = lock_state(&self.state);
            if state
                .scope_epochs
                .get(authenticated.scope())
                .copied()
                .unwrap_or(0)
                != scan_epoch
            {
                return Err(AppWidgetRuntimeError::StaleInstallationBinding);
            }
            if live.as_ref().is_some_and(|installation| {
                installation.lifecycle.status == AppInstallationStatus::Enabled
            }) {
                // The bounded scan and point read disagree without an epoch
                // change. Never delete from that ambiguous snapshot.
                return Err(AppWidgetRuntimeError::StaleInstallationBinding);
            }
            let registered_matches = state.installations.get(&key).is_some_and(|registered| {
                registered.generation == expected_generation
                    && registered.package_revision_ref == expected_package_revision_ref
            });
            let inert_matches = state.inert_installations.get(&key).is_some_and(
                |(generation, package_revision_ref)| {
                    *generation == expected_generation
                        && package_revision_ref == &expected_package_revision_ref
                },
            );
            if registered_matches || inert_matches {
                remove_installation_state(&mut state, &key, true);
            }
        }
        let state = lock_state(&self.state);
        if state
            .scope_epochs
            .get(authenticated.scope())
            .copied()
            .unwrap_or(0)
            != scan_epoch
        {
            return Err(AppWidgetRuntimeError::StaleInstallationBinding);
        }
        Ok((installations, scan_epoch))
    }

    /// Provenance for one package's exact bytes.
    ///
    /// Trusted-system provenance is granted only when this deployment's own
    /// boot admission admitted these bytes out of its read-only seed root.
    /// No adopted pin, or a digest the pin does not hold, yields `installable`
    /// — which is what keeps a `distribution: system` manifest that arrived by
    /// any other route refusing to compile.
    fn installation_provenance(
        &self,
        package_content_digest: &AppDigest,
    ) -> CompiledAppInstallationProvenance {
        let state = lock_state(&self.state);
        trusted_system_provenance(
            state.trusted_system_inventory.as_ref(),
            package_content_digest,
        )
    }

    async fn ensure_installation_registered(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation: AppInstallation,
        now: DateTime<Utc>,
    ) -> Result<(), AppWidgetRuntimeError> {
        if self.registration_or_inert_result_is_current(authenticated, &installation) {
            return Ok(());
        }
        let compile_epoch = {
            let state = lock_state(&self.state);
            state
                .scope_epochs
                .get(authenticated.scope())
                .copied()
                .unwrap_or(0)
        };
        let package_stager = self
            .package_stager
            .as_ref()
            .ok_or(AppWidgetRuntimeError::PackageAdmissionUnavailable)?;
        let package = self
            .registry
            .package_revision(authenticated, &installation.package_revision_ref, now)
            .await?
            .ok_or(AppWidgetRuntimeError::MissingPackageRevision)?;
        let staged = package_stager
            .load_staged_package(authenticated, package.content_digest.clone(), now)
            .await?;
        let candidate = staged.candidate();
        if staged.storage_digest() != &package.content_digest
            || candidate.bundle_digest() != &package.content_digest
        {
            return Err(AppWidgetRuntimeError::StalePackageBinding);
        }
        let manifest = candidate.manifest().manifest();
        validate_manifest_package_evidence(manifest, &package)?;
        let package_content_digest = package.content_digest.clone();
        // Both digests were just reconciled against the staged bytes above, so
        // this asks the pin about the bytes that will actually be compiled.
        let provenance = self.installation_provenance(&package_content_digest);
        let plan = match compile_native_manifest_widgets(
            manifest,
            installation.installation_id.clone(),
            installation.lifecycle.generation,
            installation.package_revision_ref.clone(),
            package_content_digest.clone(),
            provenance,
        ) {
            Ok(plan) => plan,
            Err(error @ AppWidgetRuntimeError::UnsupportedCompiledDeclaration(_))
            | Err(error @ AppWidgetRuntimeError::InvalidCompiledPlan(_)) => {
                let live_installation = self
                    .registry
                    .installation(authenticated, &installation.installation_id, now)
                    .await?
                    .filter(|live| {
                        live.lifecycle.status == AppInstallationStatus::Enabled
                            && live.lifecycle.generation == installation.lifecycle.generation
                            && live.package_revision_ref == installation.package_revision_ref
                    })
                    .ok_or(AppWidgetRuntimeError::StaleInstallationBinding)?;
                let live_package = self
                    .registry
                    .package_revision(authenticated, &live_installation.package_revision_ref, now)
                    .await?
                    .filter(|live| live.content_digest == package_content_digest)
                    .ok_or(AppWidgetRuntimeError::StalePackageBinding)?;
                validate_manifest_package_evidence(manifest, &live_package)?;
                let key = InstallationKey {
                    scope: authenticated.scope().clone(),
                    installation_id: installation.installation_id.clone(),
                };
                let mut state = lock_state(&self.state);
                if state
                    .scope_epochs
                    .get(authenticated.scope())
                    .copied()
                    .unwrap_or(0)
                    != compile_epoch
                    || state.installations.get(&key).is_some_and(|current| {
                        registration_blocks_candidate(
                            current.generation,
                            &current.package_revision_ref,
                            installation.lifecycle.generation,
                            &installation.package_revision_ref,
                        )
                    })
                    || state.inert_installations.get(&key).is_some_and(
                        |(generation, package_revision_ref)| {
                            registration_blocks_candidate(
                                *generation,
                                package_revision_ref,
                                installation.lifecycle.generation,
                                &installation.package_revision_ref,
                            )
                        },
                    )
                {
                    return Err(AppWidgetRuntimeError::StaleInstallationBinding);
                }
                remove_installation_state(&mut state, &key, true);
                if state.inert_installations.len() >= APP_WIDGET_MAX_INERT_REGISTRATION_CACHE
                    && !state.inert_installations.contains_key(&key)
                {
                    // Evict one stable entry instead of clearing the entire
                    // negative-admission cache. A clear-all boundary lets a
                    // fifth full scope force every invalid package in the
                    // preceding scopes through staging/compilation again on
                    // each coarse refresh.
                    let evicted = state
                        .inert_installations
                        .keys()
                        .min_by(|left, right| {
                            left.scope
                                .principal
                                .as_str()
                                .cmp(right.scope.principal.as_str())
                                .then_with(|| {
                                    left.scope
                                        .workspace
                                        .as_str()
                                        .cmp(right.scope.workspace.as_str())
                                })
                                .then_with(|| {
                                    left.installation_id
                                        .as_str()
                                        .cmp(right.installation_id.as_str())
                                })
                        })
                        .cloned();
                    if let Some(evicted) = evicted {
                        state.inert_installations.remove(&evicted);
                    }
                }
                state.inert_installations.insert(
                    key,
                    (
                        installation.lifecycle.generation,
                        installation.package_revision_ref.clone(),
                    ),
                );
                return Err(error);
            },
            Err(error) => return Err(error),
        };
        self.register_compiled_installation(authenticated, plan, now)
            .await
    }

    /// Evaluate only due indicators for one authenticated scope. The queue
    /// installs a recovery deadline before awaiting each read, so cancellation
    /// cannot permanently lose the work item. Every failed/invalid evaluation
    /// removes its current materialization before the method returns.
    pub async fn evaluate_due_indicators(
        &self,
        authenticated: &AuthenticatedAppScope,
        maximum: usize,
        now: DateTime<Utc>,
    ) -> Result<AppIndicatorEvaluationReport, AppWidgetRuntimeError> {
        let maximum = maximum.min(APP_INDICATOR_MAX_EVALUATIONS_PER_TICK);
        let mut report = AppIndicatorEvaluationReport::default();
        for _ in 0..maximum {
            let due = {
                let mut state = lock_state(&self.state);
                pop_due_indicator(&mut state, authenticated.scope(), now)
            };
            let Some((key, plan)) = due else {
                break;
            };
            let read_epoch = {
                let state = lock_state(&self.state);
                state
                    .scope_epochs
                    .get(authenticated.scope())
                    .copied()
                    .unwrap_or(0)
            };
            report.attempted = report.attempted.saturating_add(1);
            let retry_at = now + Duration::seconds(i64::from(plan.refresh_seconds));
            {
                let mut state = lock_state(&self.state);
                schedule_indicator(&mut state, key.clone(), retry_at);
            }

            let registered = {
                let state = lock_state(&self.state);
                state
                    .installations
                    .get(&InstallationKey {
                        scope: key.scope.clone(),
                        installation_id: key.installation_id.clone(),
                    })
                    .cloned()
            };
            let valid_installation = self
                .registry
                .installation(authenticated, &key.installation_id, now)
                .await
                .ok()
                .flatten()
                .filter(|installation| {
                    installation.lifecycle.status == AppInstallationStatus::Enabled
                        && installation.lifecycle.generation == key.generation
                        && registered.as_ref().is_some_and(|registered| {
                            registered.generation == key.generation
                                && registered.package_revision_ref
                                    == installation.package_revision_ref
                        })
                });
            let materialized = if valid_installation.is_some() {
                self.entity_adapter
                    .owner_query(authenticated, plan.query.clone(), now)
                    .await
                    .ok()
                    .and_then(|page| project_indicator(&key, &plan, page, now).ok().flatten())
            } else {
                None
            };
            let jitter = evaluator_jitter_seconds(&key, plan.refresh_seconds);
            let next_due = now
                + Duration::seconds(i64::from(plan.refresh_seconds))
                + Duration::seconds(jitter);
            let mut state = lock_state(&self.state);
            let plan_is_current = state
                .scope_epochs
                .get(authenticated.scope())
                .copied()
                .unwrap_or(0)
                == read_epoch
                && state.indicators.get(&key).is_some_and(|current| {
                    current.declaration_revision == plan.declaration_revision
                })
                && state
                    .installations
                    .get(&InstallationKey {
                        scope: key.scope.clone(),
                        installation_id: key.installation_id.clone(),
                    })
                    .is_some_and(|current| {
                        current.generation == key.generation
                            && registered.as_ref().is_some_and(|evaluated| {
                                current.package_revision_ref == evaluated.package_revision_ref
                            })
                    });
            if let Some(materialized) = materialized.filter(|_| plan_is_current) {
                schedule_indicator(&mut state, key.clone(), next_due);
                state.materialized_indicators.insert(key, materialized);
                report.materialized = report.materialized.saturating_add(1);
            } else if plan_is_current {
                schedule_indicator(&mut state, key.clone(), next_due);
                state.materialized_indicators.remove(&key);
                report.hidden = report.hidden.saturating_add(1);
            } else {
                // A lifecycle/generation fence may have removed this plan
                // while entity I/O was in flight. Do not resurrect its retry
                // entry after teardown.
                unschedule_indicator(&mut state, &key);
                state.materialized_indicators.remove(&key);
                report.hidden = report.hidden.saturating_add(1);
            }
        }
        Ok(report)
    }

    /// Read materialized projections only. No registry, app-store, binder, or
    /// evaluator work occurs on this endpoint path.
    pub fn indicators(
        &self,
        authenticated: &AuthenticatedAppScope,
        limit: usize,
        if_none_match: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<AppIndicatorListOutcome, AppWidgetRuntimeError> {
        authenticated.ensure_live_at(&now)?;
        let limit = limit.clamp(1, APP_INDICATOR_MAX_PAGE_ITEMS);
        let state = lock_state(&self.state);
        let mut indicators = state
            .materialized_indicators
            .iter()
            .filter(|(key, indicator)| {
                key.scope == *authenticated.scope() && indicator.expires_at > now
            })
            .filter(|(key, _)| {
                state
                    .installations
                    .get(&InstallationKey {
                        scope: key.scope.clone(),
                        installation_id: key.installation_id.clone(),
                    })
                    .is_some_and(|registered| registered.generation == key.generation)
            })
            .map(|(_, indicator)| indicator.clone())
            .collect::<Vec<_>>();
        indicators.sort_by(|left, right| {
            left.installation_id
                .as_str()
                .cmp(right.installation_id.as_str())
                .then_with(|| left.indicator_id.as_str().cmp(right.indicator_id.as_str()))
        });
        indicators.truncate(limit);
        drop(state);
        let revision = indicator_list_revision(&indicators)?;
        let etag = revision.to_string();
        if etag_matches(if_none_match, &etag) {
            return Ok(AppIndicatorListOutcome::NotModified { etag });
        }
        Ok(AppIndicatorListOutcome::Modified(
            AppIndicatorListResponse {
                schema_version: APP_WIDGET_RENDER_SCHEMA_VERSION_V1,
                revision,
                etag,
                generated_at: now,
                indicators,
            },
        ))
    }

    /// Bounded, registry-revalidated inventory seam for the future slot picker.
    pub async fn enabled_widget_inventory(
        &self,
        authenticated: &AuthenticatedAppScope,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppEnabledWidgetInventoryItem>, AppWidgetRuntimeError> {
        let limit = limit.clamp(1, APP_WIDGET_MAX_REGISTERED_PER_SCOPE);
        let candidates = {
            let state = lock_state(&self.state);
            let mut candidates = state
                .installations
                .iter()
                .filter(|(key, _)| key.scope == *authenticated.scope())
                .map(|(key, registered)| (key.clone(), registered.clone()))
                .collect::<Vec<_>>();
            candidates.sort_by(|left, right| {
                left.0
                    .installation_id
                    .as_str()
                    .cmp(right.0.installation_id.as_str())
            });
            candidates
        };
        let mut inventory = Vec::new();
        for (installation_key, registered) in candidates {
            if inventory.len() >= limit {
                break;
            }
            let Some(installation) = self
                .registry
                .installation(authenticated, &installation_key.installation_id, now)
                .await?
                .filter(|installation| {
                    installation.lifecycle.status == AppInstallationStatus::Enabled
                        && installation.lifecycle.generation == registered.generation
                        && installation.package_revision_ref == registered.package_revision_ref
                })
            else {
                continue;
            };
            let widgets = {
                let state = lock_state(&self.state);
                let mut widgets = state
                    .widgets
                    .iter()
                    .filter(|(key, _)| {
                        key.scope == *authenticated.scope()
                            && key.installation_id == installation.installation_id
                            && key.generation == installation.lifecycle.generation
                    })
                    .map(|(_, widget)| widget.clone())
                    .collect::<Vec<_>>();
                widgets
                    .sort_by(|left, right| left.widget_id.as_str().cmp(right.widget_id.as_str()));
                widgets
            };
            for widget in widgets {
                if inventory.len() >= limit {
                    break;
                }
                inventory.push(AppEnabledWidgetInventoryItem {
                    installation_id: installation.installation_id.clone(),
                    installation_generation: installation.lifecycle.generation,
                    package_revision_ref: registered.package_revision_ref.clone(),
                    package_id: registered.package.package_id.clone(),
                    package_content_digest: registered.package.content_digest.clone(),
                    trusted_system: registered.provenance.is_trusted_system(),
                    widget_id: widget.widget_id,
                    title: widget.title,
                    suggested_slots: widget.suggested_slots,
                });
            }
        }
        Ok(inventory)
    }
}

/// Decide one installation's provenance from the boot-admitted inventory.
///
/// Free rather than a method so the pins can drive the decision with a real
/// seed-root inventory and nothing else standing: what is being pinned is
/// which bytes earn system class, and that answer must not depend on how much
/// of the runtime happens to be constructed.
pub(crate) fn trusted_system_provenance(
    inventory: Option<&TrustedSystemInventoryPin>,
    package_content_digest: &AppDigest,
) -> CompiledAppInstallationProvenance {
    match inventory {
        Some(pin) if pin.admits(package_content_digest) => {
            CompiledAppInstallationProvenance::trusted_system(pin.inventory_digest().clone())
        },
        // Fail closed. Bytes this deployment's boot admission did not vouch for
        // are ordinary installable bytes, and a system manifest carrying
        // installable provenance is refused by the compiler below.
        _ => CompiledAppInstallationProvenance::installable(),
    }
}

/// Compile the exact slice-A manifest declarations into the narrowed V1 host
/// plan. This entry is pure and separable from registration: callers must
/// still pass its output through [`AppWidgetRuntime::register_compiled_installation`]
/// to reopen live registry/package identity.
///
/// Mini-frame *execution* remains outside this owner: what a declaration
/// contributes here is its native fallback — admitted only when that fallback
/// is the exact same view already bound by the widget's closed read — plus the
/// transportable entry point a client may ask a host plan for. The MiniFrame
/// capability itself is never copied into the compiled plan, because a
/// required capability decides whether the widget renders at all and this
/// escalation may only ever be additive.
pub fn compile_native_manifest_widgets(
    manifest: &AppPackageManifest,
    installation_id: AppInstallationId,
    installation_generation: u64,
    package_revision_ref: AppReference,
    package_content_digest: AppDigest,
    provenance: CompiledAppInstallationProvenance,
) -> Result<CompiledAppWidgetInstallationPlan, AppWidgetRuntimeError> {
    match (manifest.app.distribution, provenance.is_trusted_system()) {
        (AppManifestDistribution::Installable, false) | (AppManifestDistribution::System, true) => {
        },
        (AppManifestDistribution::System, false) => {
            return Err(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                "a system manifest requires digest-pinned host provenance",
            ));
        },
        (AppManifestDistribution::Installable, true) => {
            return Err(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                "trusted-system provenance cannot be attached to an installable manifest",
            ));
        },
    }
    let widgets = manifest
        .app
        .widgets
        .iter()
        .map(|widget| compile_manifest_widget(manifest, &installation_id, widget))
        .collect::<Result<Vec<_>, _>>()?;
    // An indicator declaration only says which record it is about when it
    // carries an exact selector. Without one, the best a host could do is an
    // unfiltered `LIMIT 1` over an arbitrary row, so the pre-contract shape
    // stays inert here — skipped, not refused, because refusing it would take
    // the package's widgets down with it.
    let indicators = manifest
        .app
        .indicators
        .iter()
        .filter(|indicator| indicator.selector.is_some())
        .map(|indicator| compile_manifest_indicator(manifest, &installation_id, indicator))
        .collect::<Result<Vec<_>, _>>()?;
    let plan = CompiledAppWidgetInstallationPlan {
        installation_id,
        installation_generation,
        package_revision_ref,
        package_content_digest,
        provenance,
        widgets,
        indicators,
    };
    validate_installation_plan(&plan)?;
    Ok(plan)
}

fn validate_manifest_package_evidence(
    manifest: &AppPackageManifest,
    package: &AppPackageRevision,
) -> Result<(), AppWidgetRuntimeError> {
    if package.package_id.as_str().strip_prefix("app:") != Some(manifest.name.as_str())
        || manifest.version != package.semantic_version
        || manifest.metadata.magician.app_manifest_version != package.manifest_schema_version
        || manifest.metadata.magician.app_sdk_version != package.authoring_sdk_version
        || canonical_entity_schema_digest(manifest)
            .map_err(|_| AppWidgetRuntimeError::PackageEvidenceInvalid)?
            != package.entity_schema_digest
        || canonical_view_schema_digest(manifest)
            .map_err(|_| AppWidgetRuntimeError::PackageEvidenceInvalid)?
            != package.view_schema_digest
    {
        return Err(AppWidgetRuntimeError::PackageEvidenceInvalid);
    }
    Ok(())
}

fn compile_manifest_widget(
    manifest: &AppPackageManifest,
    installation_id: &AppInstallationId,
    widget: &AppManifestWidget,
) -> Result<CompiledAppWidgetPlan, AppWidgetRuntimeError> {
    match (&widget.rendering, &widget.fallback) {
        (AppManifestWidgetRendering::Native, AppManifestWidgetFallback::Unavailable) => {},
        (
            AppManifestWidgetRendering::MiniFrame { .. },
            AppManifestWidgetFallback::View { view },
        ) if view == &widget.view && view == widget.read.view() => {},
        (AppManifestWidgetRendering::MiniFrame { .. }, _) => {
            return Err(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                "mini-frame native fallback must use the exact closed-read view",
            ));
        },
        (AppManifestWidgetRendering::Native, AppManifestWidgetFallback::View { .. }) => {
            return Err(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                "native fallback views require a separately declared fallback read",
            ));
        },
    }
    let view =
        manifest
            .app
            .views
            .get(&widget.view)
            .ok_or(AppWidgetRuntimeError::InvalidCompiledPlan(
                "widget view is absent from the manifest",
            ))?;
    let model_kind = compiled_model_kind(view.kind)?;
    let render_hints = compile_render_hints(view, widget.read.fields())?;
    let query = manifest_projection_query(
        installation_id,
        widget.read.entity(),
        widget.read.fields(),
        view,
        u32::from(widget.bounds.max_rows).min(APP_WIDGET_QUERY_MAX_ROWS),
        "widget-render",
    )?;
    // A required capability the client lacks hides the widget. Keeping
    // `mini_frame_v1` out of that set is what makes the escalation additive:
    // the declaration below travels separately, and every client that cannot
    // host a frame — or refuses this one — still renders the native model the
    // manifest made it declare.
    let required_client_capabilities = widget
        .required_capabilities
        .iter()
        .filter(|capability| **capability != AppManifestWidgetClientCapability::MiniFrameV1)
        .map(compiled_client_capability)
        .collect::<Result<Vec<_>, _>>()?;
    let mini_frame = match &widget.rendering {
        AppManifestWidgetRendering::Native => None,
        AppManifestWidgetRendering::MiniFrame { entry_point } => {
            // Manifest review already proved this names a declared
            // custom-surface entry point. An untransportable one is dropped
            // rather than refused: refusing would take the widget's reviewed
            // native content down with an escalation nobody can serve anyway.
            mini_frame_entry_point_is_transportable(entry_point).then(|| {
                AppWidgetMiniFrameDeclaration {
                    entry_point: entry_point.clone(),
                    max_height_px: APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX,
                }
            })
        },
    };
    let mut suggested_slots = Vec::with_capacity(widget.suggested_slots.len());
    for suggestion in &widget.suggested_slots {
        suggested_slots.push(AppWidgetSuggestedSlot {
            page: suggestion.page.clone(),
            region: suggestion.slot.clone(),
            system_default: suggestion.system_default,
        });
    }
    Ok(CompiledAppWidgetPlan {
        widget_id: widget.id.clone(),
        title: widget.title.clone(),
        declaration_revision: digest_json(&serde_json::to_value(widget)?)?,
        model_kind,
        render_hints,
        query,
        refresh_seconds: widget.refresh_hint.min_interval_seconds,
        maximum_model_bytes: usize::try_from(widget.bounds.max_render_bytes)
            .unwrap_or(APP_WIDGET_RENDER_MAX_MODEL_BYTES)
            .min(APP_WIDGET_RENDER_MAX_MODEL_BYTES),
        required_client_capabilities,
        fallback: AppWidgetUnsupportedFallback::Hide,
        governed_actions: widget
            .actions
            .iter()
            .map(|action| AppWidgetGovernedActionModel {
                action_id: action.governed_action.clone(),
                label: action.label.clone(),
            })
            .collect(),
        suggested_slots,
        mini_frame,
    })
}

fn manifest_projection_query(
    installation_id: &AppInstallationId,
    entity: &AppName,
    fields: &[AppName],
    view: &super::manifest::AppManifestView,
    limit: u32,
    purpose: &str,
) -> Result<AppQueryRequest, AppWidgetRuntimeError> {
    let select = fields
        .iter()
        .map(|field| AppFieldPath::parse(field.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let order = match view.kind {
        AppManifestViewKind::Tree | AppManifestViewKind::Graph => view
            .order_field
            .as_ref()
            .map(|field| (field, AppOrderDirection::Ascending)),
        AppManifestViewKind::Timeline => view
            .timestamp_field
            .as_ref()
            .map(|field| (field, AppOrderDirection::Descending)),
        AppManifestViewKind::List | AppManifestViewKind::Table => None,
        AppManifestViewKind::Board => {
            return Err(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                "board render models are not part of app_widgets_v1",
            ))
        },
    }
    .map(|(field, direction)| {
        Ok::<_, AppWidgetRuntimeError>(AppQueryOrder {
            field: AppFieldPath::parse(field.as_str())?,
            direction,
        })
    })
    .transpose()?
    .into_iter()
    .collect();
    Ok(AppQueryRequest {
        pagination: Default::default(),
        protocol_version: AppProtocolVersion::V1,
        source_installation_id: installation_id.clone(),
        entity: entity.clone(),
        select,
        predicate: None,
        order,
        cursor: None,
        limit,
        relation_expansions: Vec::new(),
        purpose: AppName::parse(purpose)?,
    })
}

/// Compile one selector-bearing indicator declaration into its host plan.
///
/// Everything the evaluator later needs to prove the choice is decided here:
/// the predicate, the row count, the order, and the kind of comparison that
/// separates a leader from its runner-up.
fn compile_manifest_indicator(
    manifest: &AppPackageManifest,
    installation_id: &AppInstallationId,
    indicator: &AppManifestIndicator,
) -> Result<CompiledAppIndicatorPlan, AppWidgetRuntimeError> {
    let selector = indicator.selector.as_ref().ok_or(
        AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
            "an indicator without an exact selector has no compiled form",
        ),
    )?;
    let entity = manifest.app.entities.get(indicator.read.entity()).ok_or(
        AppWidgetRuntimeError::InvalidCompiledPlan("indicator entity is absent from the manifest"),
    )?;
    let predicate = compile_indicator_filter(entity, selector.filter())?;
    let (selection, order) = match selector.order() {
        None => (CompiledAppIndicatorSelection::ExactRecord, Vec::new()),
        Some(declared) => {
            let field = entity.fields.get(&declared.field).ok_or(
                AppWidgetRuntimeError::InvalidCompiledPlan(
                    "indicator order field is absent from the manifest",
                ),
            )?;
            let order_kind =
                AppManifestIndicatorOrderKind::of(field)
                    .ok_or(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                    "indicator order fields must be a required, non-nullable integer or timestamp",
                ))?;
            let order_field = AppFieldPath::parse(declared.field.as_str())?;
            let direction = match declared.direction {
                AppManifestIndicatorOrderDirection::Ascending => AppOrderDirection::Ascending,
                AppManifestIndicatorOrderDirection::Descending => AppOrderDirection::Descending,
            };
            (
                CompiledAppIndicatorSelection::OrderedFirst {
                    order_field: order_field.clone(),
                    order_kind,
                },
                vec![AppQueryOrder {
                    field: order_field,
                    direction,
                }],
            )
        },
    };
    let select = indicator
        .read
        .fields()
        .iter()
        .map(|field| AppFieldPath::parse(field.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let query = AppQueryRequest {
        pagination: Default::default(),
        protocol_version: AppProtocolVersion::V1,
        source_installation_id: installation_id.clone(),
        entity: indicator.read.entity().clone(),
        select,
        predicate,
        order,
        cursor: None,
        limit: selection.read_rows(),
        relation_expansions: Vec::new(),
        purpose: AppName::parse("indicator-evaluate")?,
    };
    let field = AppFieldPath::parse(indicator.projection.field().as_str())?;
    let projection = match &indicator.projection {
        // A manifest chip carries no prefix. The compiled prefix exists for
        // server-owned plans; copying declaration text into it would put app
        // bytes in front of a host-rendered value.
        AppManifestIndicatorProjection::Chip { .. } => CompiledAppIndicatorProjection::Chip {
            field,
            prefix: None,
        },
        AppManifestIndicatorProjection::Badge { .. } => {
            CompiledAppIndicatorProjection::Badge { field }
        },
        AppManifestIndicatorProjection::State { .. } => {
            CompiledAppIndicatorProjection::State { field }
        },
    };
    Ok(CompiledAppIndicatorPlan {
        indicator_id: indicator.id.clone(),
        title: indicator.title.clone(),
        declaration_revision: digest_json(&serde_json::to_value(indicator)?)?,
        query,
        selection,
        projection,
        refresh_seconds: indicator.refresh_hint.min_interval_seconds,
        maximum_text_bytes: indicator.bounds.max_text_bytes,
        maximum_badge_value: indicator.bounds.max_badge_value,
    })
}

/// Compile a selector's equality terms into a bounded predicate arena.
///
/// Every literal is typed by the entity field it compares against, through
/// the same manifest function that admitted it, so a value can never mean one
/// thing to review and another to the store.
fn compile_indicator_filter(
    entity: &AppManifestEntity,
    filter: &[AppManifestIndicatorFilter],
) -> Result<Option<AppPredicate>, AppWidgetRuntimeError> {
    if filter.is_empty() {
        return Ok(None);
    }
    let mut nodes = Vec::with_capacity(filter.len() + 1);
    // A single term is its own root. Wrapping it in `All` would give one
    // logical predicate two encodings, and the plan digest that fences a
    // registration is taken over the encoding.
    if filter.len() > 1 {
        let children = (1..=filter.len())
            .map(|index| {
                u16::try_from(index).map_err(|_| {
                    AppWidgetRuntimeError::InvalidCompiledPlan(
                        "indicator filter is too large to encode",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        nodes.push(AppPredicateNode::All { children });
    }
    for term in filter {
        let declaration =
            entity
                .fields
                .get(&term.field)
                .ok_or(AppWidgetRuntimeError::InvalidCompiledPlan(
                    "indicator filter field is absent from the manifest",
                ))?;
        let value =
            match term
                .literal(declaration)
                .ok_or(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                "indicator filter literals must be an exact text, enum, integer or boolean value",
            ))? {
                AppManifestIndicatorLiteral::Text(text) => Value::String(text),
                AppManifestIndicatorLiteral::Enum(name) => Value::String(name.as_str().to_owned()),
                AppManifestIndicatorLiteral::Integer(number) => Value::Number(number.into()),
                AppManifestIndicatorLiteral::Boolean(flag) => Value::Bool(flag),
            };
        nodes.push(AppPredicateNode::Compare {
            field: AppFieldPath::parse(term.field.as_str())?,
            operator: AppComparisonOperator::Equal,
            value,
        });
    }
    Ok(Some(AppPredicate { root: 0, nodes }))
}

fn compile_render_hints(
    view: &super::manifest::AppManifestView,
    declared_fields: &[AppName],
) -> Result<AppWidgetRenderHints, AppWidgetRuntimeError> {
    let path = |field: &Option<AppName>| {
        field
            .as_ref()
            .map(|field| AppFieldPath::parse(field.as_str()))
            .transpose()
    };
    let structural = [
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
    if structural
        .into_iter()
        .flatten()
        .any(|field| !declared_fields.contains(field))
    {
        return Err(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
            "widget read omits a structural field required by its declared view",
        ));
    }
    let display_field = declared_fields
        .iter()
        .find(|field| {
            !structural
                .into_iter()
                .flatten()
                .any(|structural| structural == *field)
        })
        .map(|field| AppFieldPath::parse(field.as_str()))
        .transpose()?;
    Ok(AppWidgetRenderHints {
        display_field,
        partition_field: path(&view.partition_field)?,
        parent_field: path(&view.parent_field)?,
        order_field: path(&view.order_field)?,
        status_field: path(&view.status_field)?,
        timestamp_field: path(&view.timestamp_field)?,
        action_field: path(&view.action_field)?,
        actor_field: path(&view.actor_field)?,
        type_field: path(&view.type_field)?,
        target_field: path(&view.target_field)?,
    })
}

fn compiled_model_kind(
    kind: AppManifestViewKind,
) -> Result<CompiledAppWidgetModelKind, AppWidgetRuntimeError> {
    Ok(match kind {
        AppManifestViewKind::List => CompiledAppWidgetModelKind::List,
        AppManifestViewKind::Table => CompiledAppWidgetModelKind::Table,
        AppManifestViewKind::Tree => CompiledAppWidgetModelKind::Tree,
        AppManifestViewKind::Timeline => CompiledAppWidgetModelKind::Timeline,
        AppManifestViewKind::Graph => CompiledAppWidgetModelKind::Graph,
        AppManifestViewKind::Board => {
            return Err(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                "board render models are not part of app_widgets_v1",
            ))
        },
    })
}

fn compiled_client_capability(
    capability: &AppManifestWidgetClientCapability,
) -> Result<AppWidgetClientCapability, AppWidgetRuntimeError> {
    Ok(match capability {
        AppManifestWidgetClientCapability::DeclarativeListV1 => AppWidgetClientCapability::ListV1,
        AppManifestWidgetClientCapability::DeclarativeTableV1 => AppWidgetClientCapability::TableV1,
        AppManifestWidgetClientCapability::DeclarativeTreeV1 => AppWidgetClientCapability::TreeV1,
        AppManifestWidgetClientCapability::DeclarativeTimelineV1 => {
            AppWidgetClientCapability::TimelineV1
        },
        AppManifestWidgetClientCapability::DeclarativeGraphV1 => AppWidgetClientCapability::GraphV1,
        AppManifestWidgetClientCapability::GovernedActionsV1 => {
            AppWidgetClientCapability::GovernedActionsV1
        },
        // Unreachable from the compiler, which filters this capability out
        // before mapping. It stays a refusal so a future caller cannot turn
        // the additive escalation into a reason to hide the widget.
        AppManifestWidgetClientCapability::MiniFrameV1 => {
            return Err(AppWidgetRuntimeError::UnsupportedCompiledDeclaration(
                "mini-frame is an additive declaration, never a required render capability",
            ))
        },
    })
}

#[async_trait]
impl AppSlotInventoryResolver for AppWidgetRuntime {
    async fn snapshot(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<AppSlotInventorySnapshot, AppSlotInventoryError> {
        let (installations, scan_epoch) = self
            .reconcile_scope_registrations(authenticated, now)
            .await
            .map_err(|error| match error {
                AppWidgetRuntimeError::ScopePlanCapacityExceeded => AppSlotInventoryError::Invalid(
                    "enabled app inventory exceeds the widget runtime ceiling".to_owned(),
                ),
                _ => AppSlotInventoryError::Unavailable,
            })?;
        self.slot_inventory_from_installations(authenticated, installations, scan_epoch)
    }

    async fn snapshot_for_installations(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_ids: &[AppInstallationId],
        now: DateTime<Utc>,
    ) -> Result<AppSlotInventorySnapshot, AppSlotInventoryError> {
        let scan_epoch = lock_state(&self.state)
            .scope_epochs
            .get(authenticated.scope())
            .copied()
            .unwrap_or(0);
        let installations = self
            .registry
            .installations_by_ids(authenticated, installation_ids, now)
            .await
            .map_err(|_| AppSlotInventoryError::Unavailable)?;
        self.ensure_installations_registered(authenticated, &installations, now)
            .await;
        self.slot_inventory_from_installations(authenticated, installations, scan_epoch)
    }
}

impl AppWidgetRuntime {
    fn slot_inventory_from_installations(
        &self,
        authenticated: &AuthenticatedAppScope,
        installations: Vec<AppInstallation>,
        scan_epoch: u64,
    ) -> Result<AppSlotInventorySnapshot, AppSlotInventoryError> {
        let current = installations
            .into_iter()
            .map(|installation| (installation.installation_id.clone(), installation))
            .collect::<HashMap<_, _>>();

        let state = lock_state(&self.state);
        if state
            .scope_epochs
            .get(authenticated.scope())
            .copied()
            .unwrap_or(0)
            != scan_epoch
        {
            return Err(AppSlotInventoryError::Unavailable);
        }
        let mut candidates = state
            .installations
            .iter()
            .filter_map(|(key, registered)| {
                let installation = current.get(&key.installation_id)?;
                (key.scope == *authenticated.scope()
                    && installation.lifecycle.status == AppInstallationStatus::Enabled
                    && installation.lifecycle.generation == registered.generation
                    && installation.package_revision_ref == registered.package_revision_ref)
                    .then(|| (key.clone(), registered.clone(), installation.clone()))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.0
                .installation_id
                .as_str()
                .cmp(right.0.installation_id.as_str())
        });
        if candidates.len() > MAX_APP_SLOT_INVENTORY_PACKAGES {
            return Err(AppSlotInventoryError::Invalid(
                "registered package inventory exceeds its fixed ceiling".to_owned(),
            ));
        }
        let mut packages = Vec::with_capacity(candidates.len());
        let mut picker = Vec::new();
        for (key, registered, installation) in candidates {
            let binding = AppSlotPackageBinding {
                installation_id: installation.installation_id,
                package_id: registered.package.package_id.clone(),
                package_revision_ref: installation.package_revision_ref,
                package_content_digest: registered.package.content_digest.clone(),
                installation_generation: installation.lifecycle.generation,
            };
            let availability = AppSlotPackageAvailability::Enabled;
            let mut widgets = state
                .widgets
                .iter()
                .filter(|(widget_key, _)| {
                    widget_key.scope == key.scope
                        && widget_key.installation_id == key.installation_id
                        && widget_key.generation == registered.generation
                })
                .map(|(_, widget)| widget.clone())
                .collect::<Vec<_>>();
            widgets.sort_by(|left, right| left.widget_id.as_str().cmp(right.widget_id.as_str()));
            let declared_widget_ids = widgets
                .iter()
                .map(|widget| widget.widget_id.clone())
                .collect::<Vec<_>>();
            let trusted_system = registered.provenance.is_trusted_system();
            let package_state = if trusted_system {
                AppSlotPackageState::trusted_system(
                    binding.clone(),
                    availability,
                    declared_widget_ids,
                )
            } else {
                AppSlotPackageState::installable(binding.clone(), availability, declared_widget_ids)
            };
            for widget in widgets {
                let suggested_slots = widget
                    .suggested_slots
                    .iter()
                    .map(|suggestion| {
                        AppSlotSuggestion::for_page_region(
                            suggestion.page.as_str().to_owned(),
                            suggestion.region.clone(),
                            suggestion.system_default,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| AppSlotInventoryError::Invalid(error.to_string()))?;
                picker.push(AppSlotPickerCandidate {
                    widget: AppSlotWidgetBinding {
                        package: binding.clone(),
                        widget_id: widget.widget_id,
                    },
                    title: widget.title,
                    suggested_slots,
                    system_class: trusted_system,
                });
            }
            packages.push(package_state);
        }
        drop(state);
        let snapshot = AppSlotInventorySnapshot { packages, picker };
        snapshot
            .validate()
            .map_err(|error| AppSlotInventoryError::Invalid(error.to_string()))?;
        Ok(snapshot)
    }
}

fn widget_plan_is_current(
    state: &RuntimeState,
    scope: &AppScope,
    key: &DeclarationKey,
    installation: &AppInstallation,
    plan: &CompiledAppWidgetPlan,
    read_epoch: u64,
) -> bool {
    state.scope_epochs.get(scope).copied().unwrap_or(0) == read_epoch
        && state
            .widgets
            .get(key)
            .is_some_and(|current| current.declaration_revision == plan.declaration_revision)
        && state
            .installations
            .get(&InstallationKey {
                scope: key.scope.clone(),
                installation_id: key.installation_id.clone(),
            })
            .is_some_and(|current| {
                current.generation == key.generation
                    && current.package_revision_ref == installation.package_revision_ref
            })
}

fn registration_blocks_candidate(
    current_generation: u64,
    current_package_revision_ref: &AppReference,
    candidate_generation: u64,
    candidate_package_revision_ref: &AppReference,
) -> bool {
    current_generation > candidate_generation
        || (current_generation == candidate_generation
            && current_package_revision_ref != candidate_package_revision_ref)
}

fn validate_installation_plan(
    plan: &CompiledAppWidgetInstallationPlan,
) -> Result<(), AppWidgetRuntimeError> {
    if plan.installation_generation == 0 || (plan.widgets.is_empty() && plan.indicators.is_empty())
    {
        return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
            "installation generation must be positive and at least one declaration is required",
        ));
    }
    if plan.widgets.len() > APP_WIDGET_MAX_REGISTERED_PER_SCOPE
        || plan.indicators.len() > APP_INDICATOR_MAX_REGISTERED_PER_SCOPE
    {
        return Err(AppWidgetRuntimeError::ScopePlanCapacityExceeded);
    }
    let mut widget_ids = HashSet::new();
    for widget in &plan.widgets {
        if !widget_ids.insert(widget.widget_id.clone()) {
            return Err(AppWidgetRuntimeError::DuplicateDeclaration);
        }
        validate_widget_plan(widget, &plan.installation_id)?;
    }
    let mut indicator_ids = HashSet::new();
    for indicator in &plan.indicators {
        if !indicator_ids.insert(indicator.indicator_id.clone()) {
            return Err(AppWidgetRuntimeError::DuplicateDeclaration);
        }
        validate_indicator_plan(indicator, &plan.installation_id)?;
    }
    Ok(())
}

fn validate_widget_plan(
    plan: &CompiledAppWidgetPlan,
    installation_id: &AppInstallationId,
) -> Result<(), AppWidgetRuntimeError> {
    validate_query(&plan.query, installation_id, APP_WIDGET_QUERY_MAX_ROWS)?;
    validate_title(&plan.title)?;
    validate_refresh(plan.refresh_seconds)?;
    if plan.maximum_model_bytes == 0
        || plan.maximum_model_bytes > APP_WIDGET_RENDER_MAX_MODEL_BYTES
        || plan.governed_actions.len() > 8
        || plan.required_client_capabilities.len() > 8
        || plan.suggested_slots.len() > 8
    {
        return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
            "widget size, action, capability, or slot ceiling is invalid",
        ));
    }
    // A hand-built plan can reach registration without passing through the
    // manifest compiler, so the transportable bound is re-proved here rather
    // than assumed. A declaration outside it is refused, not dropped: at this
    // depth it is a caller defect, and silently serving the widget without the
    // frame it was registered with would hide that.
    if let Some(mini_frame) = &plan.mini_frame {
        if mini_frame.max_height_px == 0
            || mini_frame.max_height_px > APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX
            || !mini_frame_entry_point_is_transportable(&mini_frame.entry_point)
        {
            return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
                "mini-frame entry point or height is outside the transportable bound",
            ));
        }
    }
    if let AppWidgetUnsupportedFallback::Message { title, body } = &plan.fallback {
        validate_title(title)?;
        if body.is_empty() || body.len() > 256 || body.chars().any(char::is_control) {
            return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
                "widget fallback body is empty, contains controls, or exceeds its fixed limit",
            ));
        }
    }
    let mut action_ids = HashSet::new();
    for action in &plan.governed_actions {
        if !action_ids.insert(action.action_id.clone()) {
            return Err(AppWidgetRuntimeError::DuplicateDeclaration);
        }
        if action.label.is_empty()
            || action.label.len() > 64
            || action.label.chars().any(char::is_control)
        {
            return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
                "widget action label is empty, contains controls, or exceeds its fixed limit",
            ));
        }
    }
    Ok(())
}

fn validate_indicator_plan(
    plan: &CompiledAppIndicatorPlan,
    installation_id: &AppInstallationId,
) -> Result<(), AppWidgetRuntimeError> {
    let rows = plan.selection.read_rows();
    validate_query(&plan.query, installation_id, rows)?;
    if plan.query.limit != rows {
        return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
            "an indicator read requests exactly the rows its selection needs to prove itself",
        ));
    }
    match &plan.selection {
        // One row plus the evaluator's no-continuation rule already decides
        // the record, so an order here would be authority the plan does not
        // need and a reviewer would have to reason about.
        CompiledAppIndicatorSelection::ExactRecord => {
            if !plan.query.order.is_empty() {
                return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
                    "an exact-record indicator read carries no order",
                ));
            }
        },
        // The leader/runner-up comparison happens on the projected rows, so
        // the order field must be selected as well as ordered by.
        CompiledAppIndicatorSelection::OrderedFirst { order_field, .. } => {
            if plan.query.order.len() != 1
                || plan.query.order[0].field != *order_field
                || !plan.query.select.contains(order_field)
            {
                return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
                    "an ordered-first indicator read must order by and select its order field",
                ));
            }
        },
    }
    validate_title(&plan.title)?;
    validate_refresh(plan.refresh_seconds)?;
    let field = match &plan.projection {
        CompiledAppIndicatorProjection::Chip { field, prefix } => {
            if prefix
                .as_ref()
                .is_some_and(|prefix| prefix.len() > 32 || prefix.chars().any(char::is_control))
            {
                return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
                    "indicator prefix contains controls or exceeds its fixed limit",
                ));
            }
            field
        },
        CompiledAppIndicatorProjection::Badge { field } => field,
        CompiledAppIndicatorProjection::State { field } => field,
    };
    if plan.maximum_text_bytes == 0
        || usize::from(plan.maximum_text_bytes) > APP_INDICATOR_MAX_TEXT_BYTES
        || plan
            .maximum_badge_value
            .is_some_and(|maximum| maximum == 0 || maximum > 9_999)
        || (matches!(
            &plan.projection,
            CompiledAppIndicatorProjection::Badge { .. }
        ) != plan.maximum_badge_value.is_some())
    {
        return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
            "indicator projection bounds exceed their fixed limits",
        ));
    }
    if !plan.query.select.contains(field) {
        return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
            "indicator projection field must be selected by its read",
        ));
    }
    Ok(())
}

fn validate_query(
    query: &AppQueryRequest,
    installation_id: &AppInstallationId,
    maximum_rows: u32,
) -> Result<(), AppWidgetRuntimeError> {
    query.validate_app_contract(&AppContractLimits::default())?;
    if &query.source_installation_id != installation_id
        || query.cursor.is_some()
        || !query.relation_expansions.is_empty()
        || query.limit == 0
        || query.limit > maximum_rows
        || query.select.len() > 32
    {
        return Err(AppWidgetRuntimeError::InvalidCompiledPlan(
            "surfacing reads must be cursor-free, relation-free, installation-scoped, and ceilinged",
        ));
    }
    Ok(())
}

fn validate_registration_installation(
    installation: &AppInstallation,
    plan: &CompiledAppWidgetInstallationPlan,
) -> Result<(), AppWidgetRuntimeError> {
    if installation.lifecycle.status != AppInstallationStatus::Enabled
        || installation.installation_id != plan.installation_id
        || installation.lifecycle.generation != plan.installation_generation
        || installation.package_revision_ref != plan.package_revision_ref
    {
        return Err(AppWidgetRuntimeError::StaleInstallationBinding);
    }
    Ok(())
}

fn validate_render_request(
    request: &AppWidgetRenderBatchRequest,
) -> Result<(), AppWidgetRuntimeError> {
    if request.schema_version != APP_WIDGET_RENDER_SCHEMA_VERSION_V1 {
        return Err(AppWidgetRuntimeError::UnsupportedSchemaVersion);
    }
    if request.widgets.is_empty() || request.widgets.len() > APP_WIDGET_RENDER_MAX_PAGE_ITEMS {
        return Err(AppWidgetRuntimeError::InvalidRenderRequest);
    }
    if request.client_capabilities.len() > 16 {
        return Err(AppWidgetRuntimeError::InvalidRenderRequest);
    }
    let mut targets = HashSet::new();
    if request
        .widgets
        .iter()
        .any(|target| !targets.insert((target.installation_id.clone(), target.widget_id.clone())))
    {
        return Err(AppWidgetRuntimeError::DuplicateDeclaration);
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<(), AppWidgetRuntimeError> {
    if title.is_empty() || title.len() > 256 || title.chars().any(char::is_control) {
        Err(AppWidgetRuntimeError::InvalidCompiledPlan(
            "surfacing title is empty, contains controls, or exceeds its fixed limit",
        ))
    } else {
        Ok(())
    }
}

fn validate_refresh(seconds: u32) -> Result<(), AppWidgetRuntimeError> {
    if i64::from(seconds) < APP_SURFACING_MIN_REFRESH_SECONDS
        || i64::from(seconds) > APP_SURFACING_MAX_REFRESH_SECONDS
    {
        Err(AppWidgetRuntimeError::InvalidCompiledPlan(
            "surfacing refresh cadence is outside its fixed bounds",
        ))
    } else {
        Ok(())
    }
}

fn project_widget_model(
    plan: &CompiledAppWidgetPlan,
    page: AppQueryPage,
) -> Result<AppWidgetNativeRenderModel, AppWidgetRuntimeError> {
    if matches!(
        plan.model_kind,
        CompiledAppWidgetModelKind::Tree | CompiledAppWidgetModelKind::Graph
    ) && page.next_cursor.is_some()
    {
        return Err(AppWidgetRuntimeError::ProjectionTooLarge);
    }
    if page.envelope.value.len() > plan.query.limit as usize {
        return Err(AppWidgetRuntimeError::ProjectionTooLarge);
    }
    let rows = page
        .envelope
        .value
        .into_iter()
        .map(|row| AppWidgetRenderRow {
            entity: row.entity,
            record_id: row.record_id,
            record_revision: row.record_revision,
            fields: row.fields,
        })
        .collect::<Vec<_>>();
    let actions = plan.governed_actions.clone();
    let hints = plan.render_hints.clone();
    let model = match plan.model_kind {
        CompiledAppWidgetModelKind::Detail => AppWidgetNativeRenderModel::Detail {
            row: rows.into_iter().next(),
            hints,
            actions,
        },
        CompiledAppWidgetModelKind::List => AppWidgetNativeRenderModel::List {
            rows,
            hints,
            actions,
        },
        CompiledAppWidgetModelKind::Table => AppWidgetNativeRenderModel::Table {
            columns: plan.query.select.clone(),
            rows,
            hints,
            actions,
        },
        CompiledAppWidgetModelKind::Timeline => AppWidgetNativeRenderModel::Timeline {
            rows,
            hints,
            actions,
        },
        CompiledAppWidgetModelKind::Tree => AppWidgetNativeRenderModel::Tree {
            rows,
            hints,
            actions,
        },
        CompiledAppWidgetModelKind::Graph => AppWidgetNativeRenderModel::Graph {
            rows,
            hints,
            actions,
        },
    };
    if response_size(&model)? > plan.maximum_model_bytes {
        return Err(AppWidgetRuntimeError::ProjectionTooLarge);
    }
    Ok(model)
}

fn project_indicator(
    key: &DeclarationKey,
    plan: &CompiledAppIndicatorPlan,
    page: AppQueryPage,
    now: DateTime<Utc>,
) -> Result<Option<AppMaterializedIndicator>, AppWidgetRuntimeError> {
    // An indicator is an exact scalar projection, not "an arbitrary first
    // row". A store that returned more rows than the plan asked for has
    // already broken the bound the selection is proved against.
    if page.envelope.value.len() > plan.query.limit as usize {
        return Ok(None);
    }
    let Some(row) = selected_indicator_row(
        &plan.selection,
        &page.envelope.value,
        page.next_cursor.is_some(),
    ) else {
        return Ok(None);
    };
    let model = match &plan.projection {
        CompiledAppIndicatorProjection::Chip { field, prefix } => {
            let Some(value) = row.fields.get(field).and_then(scalar_indicator_text) else {
                return Ok(None);
            };
            let text = format!("{}{}", prefix.as_deref().unwrap_or_default(), value);
            if text.is_empty() || text.len() > usize::from(plan.maximum_text_bytes) {
                return Ok(None);
            }
            AppIndicatorRenderModel::Chip { text }
        },
        CompiledAppIndicatorProjection::Badge { field } => {
            let Some(value) = row.fields.get(field).and_then(Value::as_u64) else {
                return Ok(None);
            };
            if value == 0 {
                return Ok(None);
            }
            AppIndicatorRenderModel::Badge {
                count: value.min(u64::from(plan.maximum_badge_value.unwrap_or(9_999))) as u16,
            }
        },
        CompiledAppIndicatorProjection::State { field } => {
            let Some(label) = row.fields.get(field).and_then(scalar_indicator_text) else {
                return Ok(None);
            };
            if label.is_empty() || label.len() > usize::from(plan.maximum_text_bytes) {
                return Ok(None);
            }
            AppIndicatorRenderModel::State { label }
        },
    };
    let expires_at = now
        + Duration::seconds(i64::from(plan.refresh_seconds))
        + Duration::seconds(evaluator_jitter_seconds(key, plan.refresh_seconds));
    let revision = digest_json(&json!({
        "installation_id": key.installation_id,
        "generation": key.generation,
        "indicator_id": key.declaration_id,
        "declaration_revision": plan.declaration_revision,
        "model": &model,
    }))?;
    Ok(Some(AppMaterializedIndicator {
        installation_id: key.installation_id.clone(),
        installation_generation: key.generation,
        indicator_id: key.declaration_id.clone(),
        title: plan.title.clone(),
        revision,
        evaluated_at: now,
        expires_at,
        model,
    }))
}

/// The exact row an indicator projects, or `None` when the read did not prove
/// which row that is. Store ordering must never be the thing that decides
/// what a person sees.
///
/// `has_more` is the page's continuation cursor: rows the read matched but did
/// not return.
fn selected_indicator_row<'rows>(
    selection: &CompiledAppIndicatorSelection,
    rows: &'rows [AppRecordProjection],
    has_more: bool,
) -> Option<&'rows AppRecordProjection> {
    match selection {
        // A continuation cursor proves the selector matched a second record
        // even though the compiled query is capped at one row.
        CompiledAppIndicatorSelection::ExactRecord => {
            (rows.len() == 1 && !has_more).then(|| &rows[0])
        },
        CompiledAppIndicatorSelection::OrderedFirst {
            order_field,
            order_kind,
        } => {
            let leader = rows.first()?;
            match rows.get(1) {
                // The store returned the two best rows under the declared
                // order, so a leader that strictly outranks its runner-up
                // strictly outranks every row behind it as well.
                Some(runner_up) => indicator_order_is_strict(
                    *order_kind,
                    leader.fields.get(order_field),
                    runner_up.fields.get(order_field),
                )
                .then_some(leader),
                // A sole match has nothing to tie with. A missing second row
                // next to a continuation cursor contradicts itself, so it is
                // refused rather than trusted.
                None => (!has_more).then_some(leader),
            }
        },
    }
}

/// Whether the leader's order value strictly outranks the runner-up's under
/// the same comparison the entity store used to order them.
///
/// Anything this owner cannot compare exactly — a missing value, a null, an
/// unparseable timestamp, a number it cannot read back — is treated as a tie,
/// and a tie hides the indicator. Two spellings of one instant are a tie for
/// the same reason: the store ranked them that way, so the leader is not
/// actually ahead.
fn indicator_order_is_strict(
    kind: AppManifestIndicatorOrderKind,
    leader: Option<&Value>,
    runner_up: Option<&Value>,
) -> bool {
    let (Some(leader), Some(runner_up)) = (leader, runner_up) else {
        return false;
    };
    match kind {
        AppManifestIndicatorOrderKind::Integer => {
            match (
                indicator_order_integer(leader),
                indicator_order_integer(runner_up),
            ) {
                (Some(leader), Some(runner_up)) => leader != runner_up,
                _ => false,
            }
        },
        AppManifestIndicatorOrderKind::Timestamp => {
            let leader = leader
                .as_str()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok());
            let runner_up = runner_up
                .as_str()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok());
            match (leader, runner_up) {
                (Some(leader), Some(runner_up)) => leader != runner_up,
                _ => false,
            }
        },
    }
}

fn indicator_order_integer(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}

fn scalar_indicator_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.chars().any(char::is_control) => Some(value.clone()),
        Value::String(_) => None,
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(true) => Some("active".to_owned()),
        Value::Bool(false) | Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

fn widget_item(
    key: &DeclarationKey,
    title: &str,
    declaration_revision: &AppDigest,
    now: DateTime<Utc>,
    refresh_seconds: u32,
    render: AppWidgetRenderState,
) -> Result<AppWidgetRenderItem, AppWidgetRuntimeError> {
    let revision = digest_json(&json!({
        "installation_id": key.installation_id,
        "generation": key.generation,
        "widget_id": key.declaration_id,
        "title": title,
        "declaration_revision": declaration_revision,
        "render": &render,
    }))?;
    Ok(AppWidgetRenderItem {
        installation_id: key.installation_id.clone(),
        widget_id: key.declaration_id.clone(),
        title: Some(title.to_owned()),
        installation_generation: Some(key.generation),
        revision,
        rendered_at: now,
        refresh_after: now + Duration::seconds(i64::from(refresh_seconds)),
        render,
    })
}

fn unavailable_widget_item(
    target: AppWidgetRenderTarget,
    generation: Option<u64>,
    now: DateTime<Utc>,
    refresh_seconds: u32,
) -> Result<AppWidgetRenderItem, AppWidgetRuntimeError> {
    let revision = digest_json(&json!({
        "installation_id": &target.installation_id,
        "generation": generation,
        "widget_id": &target.widget_id,
        "state": "unavailable",
    }))?;
    Ok(AppWidgetRenderItem {
        installation_id: target.installation_id,
        widget_id: target.widget_id,
        title: None,
        installation_generation: generation,
        revision,
        rendered_at: now,
        refresh_after: now + Duration::seconds(i64::from(refresh_seconds)),
        render: AppWidgetRenderState::Unavailable,
    })
}

fn widget_refresh_slack(refresh_seconds: u32) -> Duration {
    Duration::seconds(
        (i64::from(refresh_seconds) / APP_WIDGET_REFRESH_SLACK_DIVISOR)
            .max(APP_WIDGET_REFRESH_SLACK_MIN_SECONDS),
    )
}

/// A cached render is served only while it stays fresh past the slack; one
/// about to expire is recomputed so its deadline is never near-now.
fn cached_widget_is_servable(
    fresh_until: DateTime<Utc>,
    now: DateTime<Utc>,
    refresh_seconds: u32,
) -> bool {
    fresh_until - now > widget_refresh_slack(refresh_seconds)
}

fn widget_batch_response(
    widgets: Vec<AppWidgetRenderItem>,
    now: DateTime<Utc>,
) -> Result<AppWidgetRenderBatchResponse, AppWidgetRuntimeError> {
    let refresh_after = widgets
        .iter()
        .map(|item| item.refresh_after.clone())
        .min()
        .unwrap_or(now + Duration::seconds(i64::from(APP_WIDGET_UNAVAILABLE_RETRY_SECONDS)))
        // The deadline is not part of the ETag, so the clamp never turns a
        // content change into a 304 or vice versa.
        .max(now + Duration::seconds(APP_WIDGET_REFRESH_SLACK_MIN_SECONDS));
    let revision = digest_json(&json!({
        "schema_version": APP_WIDGET_RENDER_SCHEMA_VERSION_V1,
        "widgets": widgets.iter().map(|item| json!({
            "installation_id": &item.installation_id,
            "generation": item.installation_generation,
            "widget_id": &item.widget_id,
            "revision": &item.revision,
        })).collect::<Vec<_>>(),
    }))?;
    let etag = revision.to_string();
    Ok(AppWidgetRenderBatchResponse {
        schema_version: APP_WIDGET_RENDER_SCHEMA_VERSION_V1,
        revision,
        etag,
        rendered_at: now,
        refresh_after,
        widgets,
    })
}

fn indicator_list_revision(
    indicators: &[AppMaterializedIndicator],
) -> Result<AppDigest, AppWidgetRuntimeError> {
    digest_json(&json!({
        "schema_version": APP_WIDGET_RENDER_SCHEMA_VERSION_V1,
        "indicators": indicators.iter().map(|indicator| json!({
            "installation_id": &indicator.installation_id,
            "generation": indicator.installation_generation,
            "indicator_id": &indicator.indicator_id,
            "revision": &indicator.revision,
            // Freshness is part of the HTTP representation. If an evaluation
            // renews an otherwise unchanged badge/text value, clients must
            // receive the new expiry instead of accepting a 304 against an
            // already-expired cached deadline.
            "evaluated_at": indicator.evaluated_at,
            "expires_at": indicator.expires_at,
        })).collect::<Vec<_>>(),
    }))
}

fn remove_installation_state(
    state: &mut RuntimeState,
    installation: &InstallationKey,
    remove_plans: bool,
) {
    let matching = |key: &DeclarationKey| {
        key.scope == installation.scope && key.installation_id == installation.installation_id
    };
    state.widget_cache.retain(|key, _| !matching(key));
    state
        .materialized_indicators
        .retain(|key, _| !matching(key));
    let due_keys = state
        .due_index
        .keys()
        .filter(|key| matching(key))
        .cloned()
        .collect::<Vec<_>>();
    for key in due_keys {
        unschedule_indicator(state, &key);
    }
    if remove_plans {
        state.widgets.retain(|key, _| !matching(key));
        state.indicators.retain(|key, _| !matching(key));
        state.installations.remove(installation);
        state.inert_installations.remove(installation);
    }
}

fn schedule_indicator(state: &mut RuntimeState, key: DeclarationKey, due_at: DateTime<Utc>) {
    unschedule_indicator(state, &key);
    state.next_due_sequence = state.next_due_sequence.wrapping_add(1).max(1);
    let due_key = (due_at.timestamp_millis(), state.next_due_sequence);
    state
        .due_by_scope
        .entry(key.scope.clone())
        .or_default()
        .insert(due_key, key.clone());
    state.due_index.insert(key, due_key);
}

fn unschedule_indicator(state: &mut RuntimeState, key: &DeclarationKey) {
    let Some(due_key) = state.due_index.remove(key) else {
        return;
    };
    let remove_scope = if let Some(scope_due) = state.due_by_scope.get_mut(&key.scope) {
        scope_due.remove(&due_key);
        scope_due.is_empty()
    } else {
        false
    };
    if remove_scope {
        state.due_by_scope.remove(&key.scope);
    }
}

fn pop_due_indicator(
    state: &mut RuntimeState,
    scope: &AppScope,
    now: DateTime<Utc>,
) -> Option<(DeclarationKey, CompiledAppIndicatorPlan)> {
    loop {
        let (due_key, key) = state
            .due_by_scope
            .get(scope)?
            .first_key_value()
            .map(|(due, key)| (*due, key.clone()))?;
        if due_key.0 > now.timestamp_millis() {
            return None;
        }
        state.due_by_scope.get_mut(scope)?.remove(&due_key);
        state.due_index.remove(&key);
        // Hide at the due boundary, before awaiting any read. If the worker is
        // cancelled, its recovery schedule remains but stale UI does not.
        state.materialized_indicators.remove(&key);
        if let Some(plan) = state.indicators.get(&key).cloned() {
            return Some((key, plan));
        }
    }
}

fn evaluator_jitter_seconds(key: &DeclarationKey, refresh_seconds: u32) -> i64 {
    let maximum = (refresh_seconds / 5).clamp(1, 30);
    let material = format!(
        "{}\0{}\0{}\0{}\0{}",
        key.scope.principal,
        key.scope.workspace,
        key.installation_id,
        key.generation,
        key.declaration_id
    );
    let hash = blake3::hash(material.as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&hash.as_bytes()[..8]);
    i64::from(u32::try_from(u64::from_le_bytes(bytes) % u64::from(maximum + 1)).unwrap_or(0))
}

fn digest_json(value: &Value) -> Result<AppDigest, AppWidgetRuntimeError> {
    Ok(AppDigest::blake3_canonical_json(value)?)
}

fn response_size<T: Serialize>(value: &T) -> Result<usize, AppWidgetRuntimeError> {
    Ok(serde_json::to_vec(value)?.len())
}

pub fn etag_matches(candidate: Option<&str>, current: &str) -> bool {
    candidate.is_some_and(|candidate| {
        candidate.split(',').any(|part| {
            let part = part.trim();
            part == "*" || part.strip_prefix("W/").unwrap_or(part).trim_matches('"') == current
        })
    })
}

fn lock_state(state: &StdMutex<RuntimeState>) -> std::sync::MutexGuard<'_, RuntimeState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Error)]
pub enum AppWidgetRuntimeError {
    #[error("widget runtime authentication failed: {0}")]
    Authentication(#[from] super::authority::AppAuthorityError),
    #[error("widget runtime registry failed: {0}")]
    Registry(#[from] AppRegistryError),
    #[error("widget runtime package admission failed: {0}")]
    PackageAdmission(#[from] AppPackageStagingError),
    #[error("widget runtime entity read failed: {0}")]
    Entity(#[from] AppEntityAdapterError),
    #[error("widget runtime contract failed: {0}")]
    Contract(#[from] super::models::AppContractError),
    #[error("widget runtime encoding failed: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("widget runtime installation does not exist")]
    MissingInstallation,
    #[error("widget runtime package revision does not exist")]
    MissingPackageRevision,
    #[error("widget runtime has no content-addressed package admission owner")]
    PackageAdmissionUnavailable,
    #[error("widget runtime package evidence does not match the admitted manifest")]
    PackageEvidenceInvalid,
    #[error("widget runtime installation binding is stale")]
    StaleInstallationBinding,
    #[error("widget runtime package binding is stale")]
    StalePackageBinding,
    #[error("widget runtime compiled plan is invalid: {0}")]
    InvalidCompiledPlan(&'static str),
    #[error("widget runtime declaration is not supported by this host slice: {0}")]
    UnsupportedCompiledDeclaration(&'static str),
    #[error("widget runtime declaration is duplicated")]
    DuplicateDeclaration,
    #[error("widget runtime scope plan capacity is exhausted")]
    ScopePlanCapacityExceeded,
    #[error("widget render request is invalid")]
    InvalidRenderRequest,
    #[error("widget render schema version is unsupported")]
    UnsupportedSchemaVersion,
    #[error("widget projection exceeds its declared size")]
    ProjectionTooLarge,
    #[error("widget render response exceeds its page size ceiling")]
    ResponseTooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::apps::models::AppProtocolVersion;

    fn installation_id() -> AppInstallationId {
        AppInstallationId::parse("installation-widget-test").unwrap()
    }

    fn query(limit: u32) -> AppQueryRequest {
        AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: installation_id(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("widget-render").unwrap(),
        }
    }

    fn target() -> AppWidgetRenderTarget {
        AppWidgetRenderTarget {
            installation_id: installation_id(),
            widget_id: AppName::parse("summary").unwrap(),
        }
    }

    #[test]
    fn render_request_is_batch_bounded_and_duplicate_closed() {
        let duplicate = AppWidgetRenderBatchRequest {
            schema_version: APP_WIDGET_RENDER_SCHEMA_VERSION_V1,
            client_capabilities: vec![AppWidgetClientCapability::ListV1],
            widgets: vec![target(), target()],
        };
        assert!(matches!(
            validate_render_request(&duplicate),
            Err(AppWidgetRuntimeError::DuplicateDeclaration)
        ));

        let oversized = AppWidgetRenderBatchRequest {
            schema_version: APP_WIDGET_RENDER_SCHEMA_VERSION_V1,
            client_capabilities: Vec::new(),
            widgets: (0..=APP_WIDGET_RENDER_MAX_PAGE_ITEMS)
                .map(|index| AppWidgetRenderTarget {
                    installation_id: installation_id(),
                    widget_id: AppName::parse(format!("widget-{index}")).unwrap(),
                })
                .collect(),
        };
        assert!(matches!(
            validate_render_request(&oversized),
            Err(AppWidgetRuntimeError::InvalidRenderRequest)
        ));
    }

    #[test]
    fn compiled_reads_refuse_cursors_relations_and_excess_rows() {
        let mut with_cursor = query(1);
        with_cursor.cursor = Some(AppReference::parse("cursor:copied").unwrap());
        assert!(validate_query(&with_cursor, &installation_id(), 1).is_err());

        let excessive = query(APP_WIDGET_QUERY_MAX_ROWS + 1);
        assert!(validate_query(&excessive, &installation_id(), APP_WIDGET_QUERY_MAX_ROWS).is_err());
    }

    #[test]
    fn revisions_and_etags_are_generation_fenced() {
        let key_one = DeclarationKey {
            scope: AppScope {
                principal: AppReference::parse("principal:test").unwrap(),
                workspace: AppReference::parse("workspace:test").unwrap(),
            },
            installation_id: installation_id(),
            generation: 1,
            declaration_id: AppName::parse("summary").unwrap(),
        };
        let mut key_two = key_one.clone();
        key_two.generation = 2;
        let now = DateTime::parse_from_rfc3339("2026-09-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let declaration = AppDigest::blake3(b"declaration");
        let first = widget_item(
            &key_one,
            "Summary",
            &declaration,
            now,
            APP_WIDGET_UNAVAILABLE_RETRY_SECONDS,
            AppWidgetRenderState::Unavailable,
        )
        .unwrap();
        let second = widget_item(
            &key_two,
            "Summary",
            &declaration,
            now,
            APP_WIDGET_UNAVAILABLE_RETRY_SECONDS,
            AppWidgetRenderState::Unavailable,
        )
        .unwrap();
        assert_ne!(first.revision, second.revision);
        assert!(etag_matches(
            Some(&format!("W/\"{}\"", first.revision)),
            first.revision.as_str()
        ));
        assert!(!etag_matches(
            Some(second.revision.as_str()),
            first.revision.as_str()
        ));
    }

    #[test]
    fn scalar_indicator_text_is_closed_and_text_bounded() {
        assert_eq!(scalar_indicator_text(&json!(42)).as_deref(), Some("42"));
        assert_eq!(
            scalar_indicator_text(&json!(true)).as_deref(),
            Some("active")
        );
        assert!(scalar_indicator_text(&json!(false)).is_none());
        assert!(scalar_indicator_text(&json!({"private": "shape"})).is_none());
        assert!(scalar_indicator_text(&json!("hidden\nvalue")).is_none());
        assert!(validate_title("hidden\u{0000}title").is_err());
    }

    #[test]
    fn indicator_etag_changes_when_an_unchanged_value_renews_its_expiry() {
        let evaluated_at = DateTime::parse_from_rfc3339("2026-09-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut indicator = AppMaterializedIndicator {
            installation_id: installation_id(),
            installation_generation: 1,
            indicator_id: AppName::parse("due").unwrap(),
            title: "Due".to_owned(),
            revision: AppDigest::blake3(b"same-value"),
            evaluated_at: evaluated_at.clone(),
            expires_at: evaluated_at.clone() + Duration::seconds(30),
            model: AppIndicatorRenderModel::Badge { count: 3 },
        };
        let first = indicator_list_revision(std::slice::from_ref(&indicator)).unwrap();
        indicator.evaluated_at = evaluated_at.clone() + Duration::seconds(30);
        indicator.expires_at = evaluated_at + Duration::seconds(60);
        let renewed = indicator_list_revision(std::slice::from_ref(&indicator)).unwrap();
        assert_ne!(first, renewed);
    }

    /// The fail-closed default. Until boot admission adopts an inventory pin
    /// there is no trusted-system provenance to hand out, which is exactly what
    /// keeps `compile_native_manifest_widgets` refusing every
    /// `distribution: system` manifest.
    #[test]
    fn provenance_is_installable_until_boot_admission_pins_an_inventory() {
        let digest = AppDigest::blake3(b"bytes with no boot admission behind them");
        let provenance = trusted_system_provenance(None, &digest);
        assert!(!provenance.is_trusted_system());
        assert_eq!(provenance, CompiledAppInstallationProvenance::installable());
        // The other half of the pair, so the assertion above cannot pass because
        // the predicate itself became constant.
        assert!(CompiledAppInstallationProvenance::trusted_system(digest).is_trusted_system());
    }

    fn indicator_row(record: &str, order_value: Value) -> AppRecordProjection {
        AppRecordProjection {
            entity: AppName::parse("plan").unwrap(),
            record_id: AppRecordId::parse(record).unwrap(),
            record_revision: AppRevision::new(1).unwrap(),
            fields: BTreeMap::from([
                (AppFieldPath::parse("status").unwrap(), json!("new")),
                (AppFieldPath::parse("updated_at").unwrap(), order_value),
            ]),
        }
    }

    fn ordered_first_selection() -> CompiledAppIndicatorSelection {
        CompiledAppIndicatorSelection::OrderedFirst {
            order_field: AppFieldPath::parse("updated_at").unwrap(),
            order_kind: AppManifestIndicatorOrderKind::Timestamp,
        }
    }

    /// An exact-record indicator shows a row only when the read proved that
    /// row was the only match. A continuation cursor is that proof failing.
    #[test]
    fn an_exact_record_indicator_hides_a_second_match() {
        let sole = vec![indicator_row("plan-1", json!("2026-09-04T00:00:00Z"))];
        assert!(
            selected_indicator_row(&CompiledAppIndicatorSelection::ExactRecord, &sole, false)
                .is_some()
        );
        assert!(
            selected_indicator_row(&CompiledAppIndicatorSelection::ExactRecord, &sole, true)
                .is_none()
        );
        assert!(
            selected_indicator_row(&CompiledAppIndicatorSelection::ExactRecord, &[], false)
                .is_none()
        );
    }

    /// The runner-up is the whole point of the second row: a leader that only
    /// ties it is not the leader, and the chip is hidden rather than picked by
    /// whatever order the store happened to settle on.
    #[test]
    fn an_ordered_first_indicator_must_strictly_beat_its_runner_up() {
        let selection = ordered_first_selection();
        let strict = vec![
            indicator_row("plan-1", json!("2026-09-04T00:00:00Z")),
            indicator_row("plan-2", json!("2026-09-03T00:00:00Z")),
        ];
        assert_eq!(
            selected_indicator_row(&selection, &strict, true).map(|row| row.record_id.as_str()),
            Some("plan-1"),
            "a strict leader survives rows it never read"
        );

        let tied = vec![
            indicator_row("plan-1", json!("2026-09-04T00:00:00Z")),
            // The same instant spelled differently. The store ordered these
            // as equal, so the leader is not actually ahead.
            indicator_row("plan-2", json!("2026-09-04T00:00:00+00:00")),
        ];
        assert!(selected_indicator_row(&selection, &tied, false).is_none());

        let unrankable = vec![
            indicator_row("plan-1", json!("2026-09-04T00:00:00Z")),
            indicator_row("plan-2", Value::Null),
        ];
        assert!(
            selected_indicator_row(&selection, &unrankable, false).is_none(),
            "a runner-up this owner cannot compare is a tie, not a win"
        );

        let sole = vec![indicator_row("plan-1", json!("2026-09-04T00:00:00Z"))];
        assert!(selected_indicator_row(&selection, &sole, false).is_some());
        assert!(
            selected_indicator_row(&selection, &sole, true).is_none(),
            "one row plus a continuation cursor contradicts itself"
        );
    }

    #[test]
    fn integer_order_strictness_reads_the_number_not_its_spelling() {
        let strict = indicator_order_is_strict(
            AppManifestIndicatorOrderKind::Integer,
            Some(&json!(9)),
            Some(&json!(4)),
        );
        assert!(strict);
        assert!(!indicator_order_is_strict(
            AppManifestIndicatorOrderKind::Integer,
            Some(&json!(4)),
            Some(&json!(4))
        ));
        assert!(
            !indicator_order_is_strict(
                AppManifestIndicatorOrderKind::Integer,
                Some(&json!(4)),
                Some(&json!("4"))
            ),
            "a value this owner cannot read as an integer is a tie"
        );
        assert!(!indicator_order_is_strict(
            AppManifestIndicatorOrderKind::Integer,
            Some(&json!(4)),
            None
        ));
    }

    /// The compiled plan and its selection have to agree, because the
    /// evaluator proves the selection against exactly the rows the plan asked
    /// for. A one-row ordered-first read has no runner-up to beat.
    #[test]
    fn an_indicator_plan_reads_exactly_what_its_selection_proves() {
        let plan = |selection: CompiledAppIndicatorSelection, query: AppQueryRequest| {
            CompiledAppIndicatorPlan {
                indicator_id: AppName::parse("plan-state").unwrap(),
                title: "Plan state".to_owned(),
                declaration_revision: AppDigest::blake3(b"declaration"),
                query,
                selection,
                projection: CompiledAppIndicatorProjection::State {
                    field: AppFieldPath::parse("status").unwrap(),
                },
                refresh_seconds: 60,
                maximum_text_bytes: 64,
                maximum_badge_value: None,
            }
        };
        let ordered_query = |limit: u32, select_order_field: bool| {
            let mut request = query(limit);
            request.entity = AppName::parse("plan").unwrap();
            request.select = if select_order_field {
                vec![
                    AppFieldPath::parse("status").unwrap(),
                    AppFieldPath::parse("updated_at").unwrap(),
                ]
            } else {
                vec![AppFieldPath::parse("status").unwrap()]
            };
            request.order = vec![AppQueryOrder {
                field: AppFieldPath::parse("updated_at").unwrap(),
                direction: AppOrderDirection::Descending,
            }];
            request
        };

        let mut exact = query(1);
        exact.select = vec![AppFieldPath::parse("status").unwrap()];
        assert!(validate_indicator_plan(
            &plan(CompiledAppIndicatorSelection::ExactRecord, exact.clone()),
            &installation_id()
        )
        .is_ok());

        let mut exact_two_rows = exact.clone();
        exact_two_rows.limit = 2;
        assert!(validate_indicator_plan(
            &plan(CompiledAppIndicatorSelection::ExactRecord, exact_two_rows),
            &installation_id()
        )
        .is_err());

        let mut exact_ordered = exact.clone();
        exact_ordered.order = vec![AppQueryOrder {
            field: AppFieldPath::parse("status").unwrap(),
            direction: AppOrderDirection::Ascending,
        }];
        assert!(
            validate_indicator_plan(
                &plan(CompiledAppIndicatorSelection::ExactRecord, exact_ordered),
                &installation_id()
            )
            .is_err(),
            "an exact-record read must not carry an order that could pick between rows"
        );

        assert!(validate_indicator_plan(
            &plan(ordered_first_selection(), ordered_query(2, true)),
            &installation_id()
        )
        .is_ok());
        assert!(
            validate_indicator_plan(
                &plan(ordered_first_selection(), ordered_query(1, true)),
                &installation_id()
            )
            .is_err(),
            "a one-row ordered-first read has no runner-up to beat"
        );
        assert!(
            validate_indicator_plan(
                &plan(ordered_first_selection(), ordered_query(2, false)),
                &installation_id()
            )
            .is_err(),
            "the evaluator compares the order value on the projected rows"
        );
    }

    /// The activation, end to end from declaration bytes.
    ///
    /// The pre-contract indicator shape stays inert without taking its
    /// package's widgets down with it, and the same declaration with an exact
    /// selector compiles into a plan the evaluator can prove.
    #[test]
    fn only_a_selector_bearing_indicator_compiles() {
        use crate::magician_v2::apps::manifest::{
            parse_app_manifest_frontmatter, tests::indicator_selector_document,
            tests::widget_skill_document, AppPackageLimits,
        };

        let compile = |document: String| {
            let parsed =
                parse_app_manifest_frontmatter(document.as_bytes(), &AppPackageLimits::default())
                    .expect("admitted manifest");
            let digest = AppDigest::blake3(b"pinned system bytes");
            compile_native_manifest_widgets(
                parsed.manifest(),
                installation_id(),
                1,
                AppReference::parse("package-revision:indicator-selector").unwrap(),
                digest.clone(),
                CompiledAppInstallationProvenance::trusted_system(digest),
            )
            .expect("compiled installation plan")
        };

        let inert = compile(widget_skill_document());
        assert!(
            inert.indicators.is_empty(),
            "an indicator with no selector must not compile into a live plan"
        );
        assert!(
            !inert.widgets.is_empty(),
            "an inert indicator must not take its package's widgets down with it"
        );

        let sole = compile(indicator_selector_document("        kind: sole_record"));
        assert_eq!(sole.indicators.len(), 1);
        assert_eq!(
            sole.indicators[0].selection,
            CompiledAppIndicatorSelection::ExactRecord
        );
        assert_eq!(sole.indicators[0].query.limit, 1);
        assert!(sole.indicators[0].query.predicate.is_none());

        let exact = compile(indicator_selector_document(
            "        kind: exact_record\n        filter:\n          - field: status\n            equals: done",
        ));
        let predicate = exact.indicators[0]
            .query
            .predicate
            .as_ref()
            .expect("an exact-record selector compiles to a predicate");
        assert_eq!(predicate.root, 0);
        assert!(matches!(
            predicate.nodes.as_slice(),
            [AppPredicateNode::Compare {
                operator: AppComparisonOperator::Equal,
                ..
            }]
        ));

        let ordered = compile(indicator_selector_document(
            "        kind: ordered_first\n        order:\n          field: updated_at\n          direction: descending",
        ));
        assert_eq!(
            ordered.indicators[0].selection,
            ordered_first_selection(),
            "a timestamp order compiles to the comparison the evaluator can re-check"
        );
        assert_eq!(ordered.indicators[0].query.limit, 2);
        assert_eq!(
            ordered.indicators[0].query.order[0].direction,
            AppOrderDirection::Descending
        );
    }

    /// The widget document rebuilt as a reviewed mini-frame declaration:
    /// a custom-surface entry point, the escalation capability, and the exact
    /// closed-read view as the native fallback the manifest demands.
    fn mini_frame_widget_document(entry_point: &str) -> String {
        use crate::magician_v2::apps::manifest::tests::widget_skill_document;

        widget_skill_document()
            .replace(
                "    required_features: [app_widgets_v1]\n",
                "    required_features: [app_widgets_v1, custom_surfaces_v1]\n",
            )
            .replacen(
                "app:\n",
                &format!(
                    "app:\n  permissions: [custom_surface]\n  custom_surface:\n    \
                     entry_points:\n      - route: \"{entry_point}\"\n        \
                     document: surfaces/canvas.html\n"
                ),
                1,
            )
            .replace(
                "      rendering: { kind: native }\n      fallback: { kind: unavailable }\n",
                &format!(
                    "      rendering: {{ kind: mini_frame, entry_point: \"{entry_point}\" }}\n      \
                     fallback: {{ kind: view, view: plans }}\n"
                ),
            )
            .replace(
                "      required_capabilities: [declarative_list_v1]\n",
                "      required_capabilities: [declarative_list_v1, mini_frame_v1]\n",
            )
    }

    fn compile_mini_frame_document(document: String) -> CompiledAppWidgetInstallationPlan {
        use crate::magician_v2::apps::manifest::{
            parse_app_manifest_frontmatter, AppPackageLimits,
        };

        let parsed =
            parse_app_manifest_frontmatter(document.as_bytes(), &AppPackageLimits::default())
                .expect("a reviewed mini-frame widget is an admitted manifest");
        let digest = AppDigest::blake3(b"pinned system bytes");
        compile_native_manifest_widgets(
            parsed.manifest(),
            installation_id(),
            1,
            AppReference::parse("package-revision:mini-frame").unwrap(),
            digest.clone(),
            CompiledAppInstallationProvenance::trusted_system(digest),
        )
        .expect("compiled installation plan")
    }

    /// The producer half of gate S4.
    ///
    /// Three client halves (web, iOS, Android) already refuse or admit a
    /// `mini_frame` member beside a ready model, and until this compiled the
    /// declaration none of them could ever see one. What is pinned is that the
    /// escalation stays *additive*: the capability that would hide the widget
    /// is still stripped, the native fallback still compiles, and the frame
    /// travels as a separate declaration a host plan is later minted against.
    #[test]
    fn a_reviewed_mini_frame_declaration_compiles_beside_its_native_model() {
        let plan = compile_mini_frame_document(mini_frame_widget_document("/canvas"));
        let widget = plan
            .widgets
            .iter()
            .find(|widget| widget.widget_id.as_str() == "current_plans")
            .expect("the mini-frame widget compiles");
        let declaration = widget
            .mini_frame
            .as_ref()
            .expect("a reviewed entry point reaches the compiled plan");
        assert_eq!(declaration.entry_point.as_str(), "/canvas");
        assert_eq!(
            declaration.max_height_px, APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX,
            "the host owns the frame's height ceiling; a package cannot ask for more"
        );
        assert_eq!(
            widget.required_client_capabilities,
            vec![AppWidgetClientCapability::ListV1],
            "requiring the escalation would hide the widget on every client that hosts no frames"
        );
        assert_eq!(
            widget.model_kind,
            CompiledAppWidgetModelKind::List,
            "the declared native fallback is the content a refused frame degrades to"
        );
    }

    /// The same activation against the bytes the deployment actually ships.
    ///
    /// A fixture proves the compiler; only the seed root proves that the one
    /// shipped package built for this escalation reaches a client with it. The
    /// Brainstorm canvas declares `mini_frame` rendering over a reviewed
    /// `/canvas` entry point and a declarative table fallback, and before the
    /// producer existed it compiled as an ordinary table widget with the
    /// declaration silently dropped — healthy-looking and permanently unable to
    /// show the surface it was packaged for.
    #[test]
    fn the_shipped_brainstorm_widget_advertises_its_reviewed_frame() {
        use crate::magician_v2::apps::manifest::{
            parse_app_manifest_frontmatter, AppPackageLimits,
        };

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../magician_data_v3/system/thinking_map/app/SKILL.md");
        let source = std::fs::read_to_string(&path).expect("shipped system manifest");
        let parsed = parse_app_manifest_frontmatter(
            source.as_bytes(),
            &AppPackageLimits::for_bounded_yaml(256 * 1024),
        )
        .expect("shipped manifest is admitted");
        let digest = AppDigest::blake3(source.as_bytes());
        let plan = compile_native_manifest_widgets(
            parsed.manifest(),
            installation_id(),
            1,
            AppReference::parse("package-revision:shipped-system").unwrap(),
            digest.clone(),
            CompiledAppInstallationProvenance::trusted_system(digest),
        )
        .expect("the shipped mini-frame package compiles its whole surfacing plan");

        let widget = plan
            .widgets
            .iter()
            .find(|widget| widget.widget_id.as_str() == "brainstorm_canvas")
            .expect("the shipped mini-frame widget compiles");
        let declaration = widget
            .mini_frame
            .as_ref()
            .expect("the shipped reviewed entry point must reach a client");
        assert_eq!(declaration.entry_point.as_str(), "/canvas");
        assert_eq!(
            widget.model_kind,
            CompiledAppWidgetModelKind::Table,
            "the declarative table fallback is what a client refusing the frame renders"
        );
        assert_eq!(
            widget.required_client_capabilities,
            vec![
                AppWidgetClientCapability::TableV1,
                AppWidgetClientCapability::GovernedActionsV1,
            ],
            "the shipped package declares mini_frame_v1, and requiring it would hide the \
             Brainstorm widget outright on every client that hosts no frames"
        );
    }

    /// An entry point no client would ever ask a plan for.
    ///
    /// A parameterized route has nothing to bind its parameters from inside a
    /// widget region, and every client half refuses the declaration outright —
    /// on web that costs the whole render item, not just the frame. So the
    /// producer drops it, and the widget keeps its reviewed native content.
    ///
    /// The other untransportable shape, a bare `/`, cannot be reached through
    /// a manifest here: this fixture's own root view already owns that route
    /// and manifest review refuses the overlapping custom-surface entry point.
    /// It is pinned against the registration bound below instead.
    #[test]
    fn an_untransportable_entry_point_drops_the_frame_not_the_widget() {
        let plan = compile_mini_frame_document(mini_frame_widget_document("/canvas/:id"));
        let widget = plan
            .widgets
            .iter()
            .find(|widget| widget.widget_id.as_str() == "current_plans")
            .expect("a parameterized frame route took the widget down with it");
        assert!(
            widget.mini_frame.is_none(),
            "a route no host can bind parameters for must not be advertised to a client"
        );
    }

    /// The closed door for plans that never passed through the compiler.
    ///
    /// `register_compiled_installation` accepts a caller-built plan, so the
    /// transportable bound is re-proved at registration rather than trusted.
    #[test]
    fn registration_refuses_a_mini_frame_outside_the_transportable_bound() {
        let admitted = compile_mini_frame_document(mini_frame_widget_document("/canvas"));
        let rebuild = |mini_frame: Option<AppWidgetMiniFrameDeclaration>| {
            let mut plan = admitted.clone();
            plan.widgets[0].mini_frame = mini_frame;
            validate_installation_plan(&plan)
        };

        assert!(
            rebuild(admitted.widgets[0].mini_frame.clone()).is_ok(),
            "the compiled declaration itself must clear the bound it is checked against"
        );
        for refused in [
            AppWidgetMiniFrameDeclaration {
                entry_point: AppRoute::parse("/canvas/:id").unwrap(),
                max_height_px: APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX,
            },
            AppWidgetMiniFrameDeclaration {
                entry_point: AppRoute::parse("/").unwrap(),
                max_height_px: APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX,
            },
            AppWidgetMiniFrameDeclaration {
                entry_point: AppRoute::parse("/canvas").unwrap(),
                max_height_px: APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX + 1,
            },
            AppWidgetMiniFrameDeclaration {
                entry_point: AppRoute::parse("/canvas").unwrap(),
                max_height_px: 0,
            },
        ] {
            assert!(
                matches!(
                    rebuild(Some(refused)),
                    Err(AppWidgetRuntimeError::InvalidCompiledPlan(_))
                ),
                "a plan outside the transportable bound must not register"
            );
        }
    }

    /// The wire shape all three client halves parse.
    ///
    /// `mini_frame` may appear only beside `state: "ready"`. Web's parser
    /// drops the entire render item when the member turns up on an
    /// `unsupported` or `unavailable` item, and Android decodes the batch with
    /// unknown keys refused — so the placement is a compatibility contract,
    /// not a formatting preference.
    #[test]
    fn the_mini_frame_member_rides_only_beside_a_ready_model() {
        let item = |render: AppWidgetRenderState| AppWidgetRenderItem {
            installation_id: installation_id(),
            widget_id: AppName::parse("current_plans").unwrap(),
            title: Some("Current plans".to_owned()),
            installation_generation: Some(1),
            revision: AppDigest::blake3(b"render-item"),
            rendered_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
            refresh_after: DateTime::<Utc>::from_timestamp(1_700_000_030, 0).unwrap(),
            render,
        };
        let model = || AppWidgetNativeRenderModel::List {
            rows: Vec::new(),
            hints: AppWidgetRenderHints::default(),
            actions: Vec::new(),
        };

        let ready = serde_json::to_value(item(AppWidgetRenderState::Ready {
            model: model(),
            mini_frame: Some(AppWidgetMiniFrameDeclaration {
                entry_point: AppRoute::parse("/canvas").unwrap(),
                max_height_px: APP_WIDGET_MINI_FRAME_MAX_HEIGHT_PX,
            }),
        }))
        .expect("a render item serializes");
        assert_eq!(ready["state"], json!("ready"));
        assert_eq!(
            ready["mini_frame"],
            json!({ "entry_point": "/canvas", "max_height_px": 480 }),
            "the member sits beside `state`, where every client half looks for it"
        );

        let unframed = serde_json::to_value(item(AppWidgetRenderState::Ready {
            model: model(),
            mini_frame: None,
        }))
        .expect("a render item serializes");
        assert!(
            unframed.get("mini_frame").is_none(),
            "the ordinary widget must not gain a key older clients never expected"
        );

        let unavailable = serde_json::to_value(item(AppWidgetRenderState::Unavailable))
            .expect("a render item serializes");
        assert!(unavailable.get("mini_frame").is_none());
    }

    /// The activation the deployment actually ships, against the seed bytes.
    ///
    /// The selector gate is only half of it. A shipped indicator that declares
    /// no selector compiles to nothing and takes nothing down with it, so the
    /// package looks healthy while its chip can never appear — the same silent
    /// inertness the gate exists to remove, one layer up. Only the real bytes
    /// can catch that, which is why this reads the seed root rather than a
    /// fixture: a fixture would keep passing after the manifest regressed.
    #[test]
    fn the_shipped_system_indicators_compile_into_provable_plans() {
        use crate::magician_v2::apps::manifest::{
            parse_app_manifest_frontmatter, AppPackageLimits,
        };

        let compile = |package: &str| {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../magician_data_v3/system")
                .join(package)
                .join("app/SKILL.md");
            let source = std::fs::read_to_string(&path).expect("shipped system manifest");
            let parsed = parse_app_manifest_frontmatter(
                source.as_bytes(),
                &AppPackageLimits::for_bounded_yaml(256 * 1024),
            )
            .expect("shipped manifest is admitted");
            // Boot admission mints provenance from the seed bundle's own
            // digest. Here the digest only has to be the same one on both
            // sides of the compile, which is what the class check compares.
            let digest = AppDigest::blake3(source.as_bytes());
            compile_native_manifest_widgets(
                parsed.manifest(),
                installation_id(),
                1,
                AppReference::parse("package-revision:shipped-system").unwrap(),
                digest.clone(),
                CompiledAppInstallationProvenance::trusted_system(digest),
            )
            .expect("a shipped system manifest compiles its whole surfacing plan")
        };

        // `capture_session` also holds ended sessions inside their retention
        // window and rows belonging to other scopes, so the chip has to name
        // the live one *in this scope*; an unfiltered read would have let store
        // order decide which capture the operator was being told about, and a
        // `live`-only read would have let another principal's recording decide
        // it — a cross-scope row keeps `live` and `status` and drops only the
        // meeting content.
        let meetings = compile("meetings");
        assert!(
            !meetings.widgets.is_empty(),
            "a refused widget declaration takes the package's indicators with it"
        );
        assert_eq!(
            meetings.indicators.len(),
            1,
            "the shipped capture chip must reach a compiled plan"
        );
        let capture = &meetings.indicators[0];
        assert_eq!(capture.indicator_id.as_str(), "capture_state");
        assert_eq!(
            capture.selection,
            CompiledAppIndicatorSelection::ExactRecord
        );
        assert_eq!(capture.query.limit, 1);
        let predicate = capture
            .query
            .predicate
            .as_ref()
            .expect("the capture chip narrows to this scope's live session");
        assert_eq!(predicate.root, 0);
        assert!(
            matches!(
                predicate.nodes.as_slice(),
                [
                    AppPredicateNode::All { children },
                    AppPredicateNode::Compare {
                        field: live,
                        operator: AppComparisonOperator::Equal,
                        value: live_value,
                    },
                    AppPredicateNode::Compare {
                        field: in_scope,
                        operator: AppComparisonOperator::Equal,
                        value: in_scope_value,
                    },
                ] if children == &[1, 2]
                    && live.as_str() == "live"
                    && live_value == &Value::Bool(true)
                    && in_scope.as_str() == "in_scope"
                    && in_scope_value == &Value::Bool(true)
            ),
            "the capture chip must compile to the live-AND-in-scope conjunction: a live-only \
             filter renders another principal's recording as this operator's capture state, and \
             lets a second scope's capture blank this operator's own chip"
        );

        // The policy entity is a declared singleton written at one record id,
        // so the chip names it with `sole_record` and needs no predicate — and
        // a second policy row hides the chip rather than choosing between them.
        let town_square = compile("town_square");
        assert!(!town_square.widgets.is_empty());
        assert_eq!(town_square.indicators.len(), 1);
        let autonomy = &town_square.indicators[0];
        assert_eq!(autonomy.indicator_id.as_str(), "autonomy_state");
        assert_eq!(
            autonomy.selection,
            CompiledAppIndicatorSelection::ExactRecord
        );
        assert_eq!(autonomy.query.limit, 1);
        assert!(autonomy.query.predicate.is_none());
    }

    #[test]
    fn negative_admission_accepts_only_a_monotonic_newer_binding() {
        let old = AppReference::parse("package-revision:old").unwrap();
        let next = AppReference::parse("package-revision:next").unwrap();
        assert!(!registration_blocks_candidate(4, &old, 5, &next));
        assert!(registration_blocks_candidate(6, &next, 5, &old));
        assert!(registration_blocks_candidate(5, &old, 5, &next));
        assert!(!registration_blocks_candidate(5, &next, 5, &next));
    }

    #[test]
    fn a_cached_render_near_expiry_is_recomputed_not_served() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        // 60 s refresh: slack is 12 s.
        assert_eq!(widget_refresh_slack(60), Duration::seconds(12));
        assert!(cached_widget_is_servable(now + Duration::seconds(13), now, 60));
        assert!(!cached_widget_is_servable(now + Duration::seconds(12), now, 60));
        assert!(!cached_widget_is_servable(
            now + Duration::milliseconds(3),
            now,
            60
        ));
        // Short refresh: the 5 s surfacing floor dominates.
        assert_eq!(widget_refresh_slack(5), Duration::seconds(5));
        assert_eq!(widget_refresh_slack(20), Duration::seconds(5));
        assert!(!cached_widget_is_servable(now + Duration::seconds(5), now, 5));
    }

    #[test]
    fn the_batch_deadline_is_never_near_now_and_stays_out_of_the_etag() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut item = unavailable_widget_item(target(), Some(1), now, 30).unwrap();
        item.rendered_at = now - Duration::seconds(30);
        item.refresh_after = now + Duration::milliseconds(4);
        let near = widget_batch_response(vec![item.clone()], now).unwrap();
        assert_eq!(
            near.refresh_after,
            now + Duration::seconds(APP_WIDGET_REFRESH_SLACK_MIN_SECONDS),
            "a near-now minimum is clamped to the surfacing floor"
        );
        assert!(near.refresh_after - near.rendered_at >= Duration::seconds(5));

        item.refresh_after = now + Duration::seconds(40);
        let far = widget_batch_response(vec![item], now).unwrap();
        assert_eq!(far.refresh_after, now + Duration::seconds(40));
        assert_eq!(
            near.etag, far.etag,
            "deadline movement alone never changes the representation"
        );

        let empty = widget_batch_response(Vec::new(), now).unwrap();
        assert_eq!(
            empty.refresh_after,
            now + Duration::seconds(i64::from(APP_WIDGET_UNAVAILABLE_RETRY_SECONDS))
        );
    }
}
