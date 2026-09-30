//! Deterministic compiler from an admitted app view to the existing MUIJ
//! renderer contract.
//!
//! This module owns no records, cursors or route authority. It emits an empty
//! render skeleton whose live rows are hydrated later through the canonical
//! app entity-query service. Keeping compilation pure makes the surface
//! identity reproducible and prevents package content from smuggling record
//! snapshots or an alternate query dialect into MUIJ.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use super::{
    manifest::{
        canonical_view_schema_digest, tree_display_field, AppManifestEntity, AppManifestField,
        AppManifestSurfaceComponent, AppManifestView, AppManifestViewKind, AppPackageManifest,
        AppRoute, CanonicalAppManifest, APP_SURFACE_MAX_DECLARATIVE_BYTES,
        APP_SURFACE_MAX_DECLARATIVE_COMPONENTS, APP_SURFACE_MAX_DECLARATIVE_DEPTH,
    },
    models::{
        decode_bounded_json_value, AppComparisonOperator, AppContractError, AppContractLimits,
        AppDigest, AppFieldPath, AppInstallationId, AppName, AppOrderDirection, AppPredicate,
        AppPredicateNode, AppProtocolVersion, AppQueryOrder, AppQueryRequest, AppRecordId,
        AppReference, AppRevision, ValidateAppContract,
    },
    package_staging::StagedAppPackage,
    records::{AppPackageRevision, AppSchemaRevision, AppSurfaceBinding, AppSurfaceStatus},
    registry::canonical_package_revision_ref,
    schema_compiler::{
        canonical_entity_schema_digest, source_entity_schema_digest_from_revision,
        AppSchemaCompilerError,
    },
};
use crate::magician_v2::{
    gaui::muij::{DefaultComponentRegistry, MuijComponent, MuijDocument, MuijValidationError},
    json_traversal::canonical_json_bytes,
};

pub const APP_SURFACE_COMPILER_VERSION: u32 = 1;
pub const DEFAULT_SURFACE_PAGE_SIZE: u32 = 25;
pub const DEFAULT_TREE_MAX_DEPTH: u16 = 3;
pub const MAX_SURFACE_FIELDS: usize = 64;
pub const APP_QUERY_SOURCE: &str = "app_entity_query_v1";
const APP_SURFACE_QUERY_PURPOSE: &str = "surface_hydration";

/// Opaque, non-deserializable evidence that one re-admitted immutable package
/// snapshot, its registry record and its compiled schema all name the same
/// installation/package/schema identity.
///
/// Surface compilation accepts this proof instead of independent caller-
/// supplied IDs and digests. That makes it structurally impossible to pair a
/// valid manifest with another package's reference at the compiler boundary.
#[derive(Debug)]
pub struct VerifiedAppSurfaceSource {
    manifest: CanonicalAppManifest,
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    schema_created_at: DateTime<Utc>,
    entity_schema_digest: AppDigest,
    view_schema_digest: AppDigest,
}

impl VerifiedAppSurfaceSource {
    pub fn from_registry_snapshot(
        staged_package: &StagedAppPackage,
        package_revision: &AppPackageRevision,
        schema_revision: &AppSchemaRevision,
    ) -> Result<Self, AppSurfaceCompilerError> {
        package_revision.validate_app_contract(&AppContractLimits::default())?;
        schema_revision.validate_app_contract(&AppContractLimits::default())?;
        if staged_package.storage_digest() != &package_revision.content_digest
            || staged_package.candidate().bundle_digest() != &package_revision.content_digest
        {
            return Err(AppSurfaceCompilerError::PackageEvidenceMismatch(
                "staged package bytes do not match the registry package content digest",
            ));
        }

        let manifest = staged_package.candidate().manifest();
        let declared = manifest.manifest();
        if package_revision.semantic_version != declared.version
            || package_revision.manifest_schema_version
                != declared.metadata.magician.app_manifest_version
            || package_revision.authoring_sdk_version != declared.metadata.magician.app_sdk_version
        {
            return Err(AppSurfaceCompilerError::PackageEvidenceMismatch(
                "registry package metadata does not match the re-admitted manifest",
            ));
        }
        validate_manifest_schema_identity(
            declared,
            &package_revision.entity_schema_digest,
            &package_revision.view_schema_digest,
        )?;

        let package_revision_ref =
            canonical_package_revision_ref(package_revision).map_err(|_| {
                AppSurfaceCompilerError::PackageEvidenceMismatch(
                    "registry package revision has no canonical identity",
                )
            })?;
        if schema_revision.package_revision_ref != package_revision_ref {
            return Err(AppSurfaceCompilerError::SchemaEvidenceMismatch(
                "compiled schema names another package revision",
            ));
        }
        if source_entity_schema_digest_from_revision(schema_revision)?
            != package_revision.entity_schema_digest
        {
            return Err(AppSurfaceCompilerError::SchemaEvidenceMismatch(
                "compiled schema source does not match the immutable entity schema",
            ));
        }

        Ok(Self {
            manifest: manifest.clone(),
            installation_id: schema_revision.installation_id.clone(),
            package_revision_ref,
            schema_revision: schema_revision.revision,
            schema_created_at: schema_revision.created_at,
            entity_schema_digest: package_revision.entity_schema_digest.clone(),
            view_schema_digest: package_revision.view_schema_digest.clone(),
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn for_test(
        manifest: CanonicalAppManifest,
        installation_id: AppInstallationId,
        package_revision_ref: AppReference,
        schema_revision: AppRevision,
        schema_created_at: DateTime<Utc>,
        entity_schema_digest: AppDigest,
        view_schema_digest: AppDigest,
    ) -> Self {
        Self {
            manifest,
            installation_id,
            package_revision_ref,
            schema_revision,
            schema_created_at,
            entity_schema_digest,
            view_schema_digest,
        }
    }

    pub fn manifest(&self) -> &CanonicalAppManifest {
        &self.manifest
    }

    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub fn package_revision_ref(&self) -> &AppReference {
        &self.package_revision_ref
    }

    pub fn schema_revision(&self) -> AppRevision {
        self.schema_revision
    }
}

/// Identifies the interaction adapter that owns a rendered MUIJ document.
///
/// App surfaces deliberately do not use the existing agent `ui.interaction`
/// route. The enum leaves an explicit versioned seam for future non-agent
/// owners without changing `MuijDocument` for existing consumers.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppSurfaceInteractionMode {
    App,
}

/// Read-only wire payload for one compiled app surface.
///
/// Fields are private so production callers cannot construct an envelope that
/// bypasses MUIJ validation. HTTP adapters can serialize this type directly.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppSurfaceEnvelope {
    installation_id: AppInstallationId,
    surface_revision: AppRevision,
    view_id: AppName,
    muij_document: MuijDocument,
    interaction_mode: AppSurfaceInteractionMode,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAppSurfaceEnvelope {
    installation_id: AppInstallationId,
    surface_revision: AppRevision,
    view_id: AppName,
    muij_document: MuijDocument,
    interaction_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppSurfaceViewBinding {
    entity: AppName,
    #[serde(rename = "viewKind")]
    view_kind: AppManifestViewKind,
    fields: Vec<AppName>,
    #[serde(rename = "fieldBindings")]
    field_bindings: Vec<AppSurfaceFieldBinding>,
    #[serde(
        rename = "routeBindings",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    route_bindings: Vec<AppSurfaceRouteBinding>,
    #[serde(
        rename = "surfaceComponents",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    surface_components: Vec<AppSurfaceComponentBinding>,
    #[serde(
        rename = "labelField",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    label_field: Option<AppName>,
    #[serde(
        rename = "partitionField",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    partition_field: Option<AppName>,
    #[serde(
        rename = "parentField",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    parent_field: Option<AppName>,
    #[serde(
        rename = "orderField",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    order_field: Option<AppName>,
    #[serde(
        rename = "statusField",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    status_field: Option<AppName>,
    #[serde(rename = "maxDepth", default, skip_serializing_if = "Option::is_none")]
    max_depth: Option<u16>,
    #[serde(
        rename = "timestampField",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    timestamp_field: Option<AppName>,
    #[serde(
        rename = "actionField",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    action_field: Option<AppName>,
    #[serde(
        rename = "actorField",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    actor_field: Option<AppName>,
    #[serde(rename = "typeField", default, skip_serializing_if = "Option::is_none")]
    type_field: Option<AppName>,
    #[serde(
        rename = "targetField",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    target_field: Option<AppName>,
    #[serde(
        rename = "defaultActor",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    default_actor: Option<String>,
    #[serde(
        rename = "defaultType",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    default_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AppSurfaceComponentBinding {
    Detail {
        id: AppName,
        label: String,
        fields: Vec<AppName>,
    },
    Form {
        id: AppName,
        label: String,
        fields: Vec<AppName>,
    },
    Section {
        id: AppName,
        label: String,
        children: Vec<AppSurfaceComponentBinding>,
    },
    List {
        id: AppName,
        label: String,
        fields: Vec<AppName>,
    },
    Table {
        id: AppName,
        label: String,
        columns: Vec<AppName>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AppSurfaceFieldKind {
    Text,
    Markdown,
    Integer,
    Decimal,
    Boolean,
    Timestamp,
    Enum,
    Reference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppSurfaceFieldBinding {
    field: AppName,
    kind: AppSurfaceFieldKind,
    required: bool,
    nullable: bool,
    sortable: bool,
    #[serde(
        rename = "allowedValues",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    allowed_values: Vec<AppName>,
    #[serde(
        rename = "referenceEntity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    reference_entity: Option<AppName>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AppSurfaceRouteScalarKind {
    Text,
    Integer,
    Decimal,
    Boolean,
    Enum,
    Reference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppSurfaceRouteBinding {
    parameter: AppName,
    field: AppName,
    #[serde(rename = "scalarKind")]
    scalar_kind: AppSurfaceRouteScalarKind,
    #[serde(
        rename = "allowedValues",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    allowed_values: Vec<AppName>,
}

/// Compiler-owned query template recovered from a validated surface envelope.
/// Cursor and installation authority are supplied only by the authenticated
/// route owner when the request is executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSurfaceQueryPlan {
    entity: AppName,
    view_kind: AppManifestViewKind,
    select: Vec<AppFieldPath>,
    visible_fields: Vec<AppName>,
    order: Vec<AppQueryOrder>,
    route_bindings: Vec<AppSurfaceRouteBinding>,
    field_bindings: Vec<AppSurfaceFieldBinding>,
    mutation_allowed: bool,
}

#[derive(Debug, Error)]
pub enum AppSurfaceQueryPlanError {
    #[error("app surface route parameters are invalid")]
    InvalidRouteParameters,
    #[error("app surface read intent is invalid")]
    InvalidReadIntent,
    #[error(transparent)]
    Contract(#[from] AppContractError),
}

impl AppSurfaceEnvelope {
    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub fn surface_revision(&self) -> AppRevision {
        self.surface_revision
    }

    pub fn view_id(&self) -> &AppName {
        &self.view_id
    }

    pub fn muij_document(&self) -> &MuijDocument {
        &self.muij_document
    }

    pub fn interaction_mode(&self) -> AppSurfaceInteractionMode {
        self.interaction_mode
    }

    pub fn decode_persisted(bytes: &[u8]) -> Result<Self, AppSurfaceCompilerError> {
        let value = decode_bounded_json_value(bytes, &AppContractLimits::default())?;
        let persisted: PersistedAppSurfaceEnvelope = serde_json::from_value(value)
            .map_err(|error| AppSurfaceCompilerError::PersistedEnvelope(error.to_string()))?;
        if persisted.interaction_mode != "app" {
            return Err(AppSurfaceCompilerError::PersistedEnvelope(
                "interaction_mode is not app".to_owned(),
            ));
        }
        Ok(Self {
            installation_id: persisted.installation_id,
            surface_revision: persisted.surface_revision,
            view_id: persisted.view_id,
            muij_document: persisted.muij_document,
            interaction_mode: AppSurfaceInteractionMode::App,
        })
    }

    pub fn query_plan(&self) -> Result<AppSurfaceQueryPlan, AppSurfaceCompilerError> {
        let expected_query = format!("view:{}", self.view_id.as_str());
        let mut stack = self
            .muij_document
            .layout
            .iter()
            .collect::<Vec<&MuijComponent>>();
        let mut binding = None;
        while let Some(component) = stack.pop() {
            stack.extend(component.children.iter());
            if component.source.as_deref() != Some(APP_QUERY_SOURCE)
                || component.query.as_deref() != Some(expected_query.as_str())
            {
                continue;
            }
            if binding.is_some() {
                return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                    "surface contains more than one canonical query component".to_owned(),
                ));
            }
            let raw = component.props.get("viewBinding").cloned().ok_or_else(|| {
                AppSurfaceCompilerError::InvalidQueryPlan(
                    "canonical query component has no viewBinding".to_owned(),
                )
            })?;
            binding = Some(
                serde_json::from_value::<AppSurfaceViewBinding>(raw).map_err(|error| {
                    AppSurfaceCompilerError::InvalidQueryPlan(error.to_string())
                })?,
            );
        }
        AppSurfaceQueryPlan::from_view_binding(binding.ok_or_else(|| {
            AppSurfaceCompilerError::InvalidQueryPlan(
                "surface contains no canonical query component".to_owned(),
            )
        })?)
    }
}

impl AppSurfaceQueryPlan {
    fn from_view_binding(binding: AppSurfaceViewBinding) -> Result<Self, AppSurfaceCompilerError> {
        binding.validate()?;
        // Read-only kinds: timelines (the historical case) and graphs, whose
        // family interactions (select/expand/focus/reveal) carry no mutations.
        let mutation_allowed = !matches!(
            binding.view_kind,
            AppManifestViewKind::Timeline | AppManifestViewKind::Graph
        );
        let visible_fields = binding.fields.clone();
        // Editable surfaces need the complete entity payload to edit safely:
        // rendering can keep its declared column projection, but a form must
        // not infer absent non-projected fields or overwrite them with empty
        // values. Read-only timelines and graphs retain their intentionally
        // narrow projection.
        let selected_fields = if mutation_allowed {
            binding
                .field_bindings
                .iter()
                .map(|field| &field.field)
                .collect::<Vec<_>>()
        } else {
            binding.fields.iter().collect::<Vec<_>>()
        };
        let select = selected_fields
            .iter()
            .map(|field| AppFieldPath::parse(field.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        let order_field = match binding.view_kind {
            AppManifestViewKind::Tree => binding
                .order_field
                .as_ref()
                .map(|field| (field, AppOrderDirection::Ascending)),
            AppManifestViewKind::Graph => binding
                .order_field
                .as_ref()
                .map(|field| (field, AppOrderDirection::Ascending)),
            AppManifestViewKind::Timeline => binding
                .timestamp_field
                .as_ref()
                .map(|field| (field, AppOrderDirection::Descending)),
            AppManifestViewKind::List | AppManifestViewKind::Table => None,
            AppManifestViewKind::Board => {
                return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                    "Board is not a supported V1 surface binding".to_owned(),
                ));
            },
        };
        let order = order_field
            .map(|(field, direction)| {
                Ok::<AppQueryOrder, AppSurfaceCompilerError>(AppQueryOrder {
                    field: AppFieldPath::parse(field.as_str())?,
                    direction,
                })
            })
            .transpose()?
            .into_iter()
            .collect();
        Ok(Self {
            entity: binding.entity,
            view_kind: binding.view_kind,
            select,
            visible_fields,
            order,
            route_bindings: binding.route_bindings,
            field_bindings: binding.field_bindings,
            mutation_allowed,
        })
    }

    pub fn entity(&self) -> &AppName {
        &self.entity
    }

    pub fn mutation_allowed(&self) -> bool {
        self.mutation_allowed
    }

    pub fn requires_complete_page(&self) -> bool {
        // Trees and graphs must assemble from the whole entity set — a paged
        // subset cannot assemble parent→child edges any more than tree depth.
        matches!(
            self.view_kind,
            AppManifestViewKind::Tree | AppManifestViewKind::Graph
        )
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn tree_for_complete_page_test() -> Self {
        Self {
            entity: AppName::parse("item").expect("test entity name is valid"),
            view_kind: AppManifestViewKind::Tree,
            select: Vec::new(),
            visible_fields: Vec::new(),
            order: Vec::new(),
            route_bindings: Vec::new(),
            field_bindings: Vec::new(),
            mutation_allowed: true,
        }
    }

    pub fn request(
        &self,
        installation_id: AppInstallationId,
        cursor: Option<AppReference>,
        route_parameters: &BTreeMap<String, String>,
        sort: Option<(&AppName, AppOrderDirection)>,
    ) -> Result<AppQueryRequest, AppSurfaceQueryPlanError> {
        let order = match sort {
            Some((field, direction)) => {
                let binding = self
                    .field_bindings
                    .iter()
                    .find(|binding| &binding.field == field)
                    .filter(|binding| {
                        binding.sortable
                            && self
                                .visible_fields
                                .iter()
                                .any(|visible| visible == &binding.field)
                    })
                    .ok_or(AppSurfaceQueryPlanError::InvalidReadIntent)?;
                vec![AppQueryOrder {
                    field: AppFieldPath::parse(binding.field.as_str())?,
                    direction,
                }]
            },
            None => self.order.clone(),
        };
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: installation_id,
            entity: self.entity.clone(),
            select: self.select.clone(),
            predicate: route_predicate(&self.route_bindings, route_parameters)?,
            order,
            cursor,
            limit: DEFAULT_SURFACE_PAGE_SIZE,
            relation_expansions: Vec::new(),
            purpose: AppName::parse(APP_SURFACE_QUERY_PURPOSE)?,
        };
        request.validate_app_contract(&AppContractLimits::default())?;
        Ok(request)
    }

    fn validate_route_template(&self, route: &str) -> Result<(), AppSurfaceCompilerError> {
        let route = AppRoute::parse(route).map_err(|_| {
            AppSurfaceCompilerError::InvalidQueryPlan(
                "surface query plan has an invalid route template".to_owned(),
            )
        })?;
        if !route.parameter_names().eq(self
            .route_bindings
            .iter()
            .map(|binding| binding.parameter.as_str()))
        {
            return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                "surface route parameters differ from compiler-owned query bindings".to_owned(),
            ));
        }
        Ok(())
    }
}

fn route_predicate(
    bindings: &[AppSurfaceRouteBinding],
    parameters: &BTreeMap<String, String>,
) -> Result<Option<AppPredicate>, AppSurfaceQueryPlanError> {
    if bindings.len() != parameters.len() {
        return Err(AppSurfaceQueryPlanError::InvalidRouteParameters);
    }
    let mut comparisons = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let raw = parameters
            .get(binding.parameter.as_str())
            .ok_or(AppSurfaceQueryPlanError::InvalidRouteParameters)?;
        comparisons.push(AppPredicateNode::Compare {
            field: AppFieldPath::parse(binding.field.as_str())?,
            operator: AppComparisonOperator::Equal,
            value: route_scalar_value(binding, raw)
                .ok_or(AppSurfaceQueryPlanError::InvalidRouteParameters)?,
        });
    }
    match comparisons.len() {
        0 => Ok(None),
        1 => Ok(Some(AppPredicate {
            root: 0,
            nodes: comparisons,
        })),
        count => {
            let children = (1..=count)
                .map(|index| {
                    u16::try_from(index)
                        .map_err(|_| AppSurfaceQueryPlanError::InvalidRouteParameters)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut nodes = Vec::with_capacity(count.saturating_add(1));
            nodes.push(AppPredicateNode::All { children });
            nodes.extend(comparisons);
            Ok(Some(AppPredicate { root: 0, nodes }))
        },
    }
}

fn route_scalar_value(binding: &AppSurfaceRouteBinding, raw: &str) -> Option<Value> {
    match binding.scalar_kind {
        AppSurfaceRouteScalarKind::Text => Some(Value::String(raw.to_owned())),
        AppSurfaceRouteScalarKind::Integer => raw
            .parse::<i64>()
            .ok()
            .filter(|value| value.to_string() == raw)
            .map(serde_json::Number::from)
            .or_else(|| {
                raw.parse::<u64>()
                    .ok()
                    .filter(|value| value.to_string() == raw)
                    .map(serde_json::Number::from)
            })
            .map(Value::Number),
        AppSurfaceRouteScalarKind::Decimal => raw
            .parse::<serde_json::Number>()
            .ok()
            .filter(|value| value.to_string() == raw)
            .map(Value::Number),
        AppSurfaceRouteScalarKind::Boolean => match raw {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        AppSurfaceRouteScalarKind::Enum => AppName::parse(raw)
            .ok()
            .filter(|value| binding.allowed_values.contains(value))
            .map(|value| Value::String(value.to_string())),
        AppSurfaceRouteScalarKind::Reference => AppRecordId::parse(raw)
            .ok()
            .map(|value| Value::String(value.to_string())),
    }
}

impl AppSurfaceViewBinding {
    fn collection(
        entity: &AppName,
        view_kind: AppManifestViewKind,
        fields: Vec<AppName>,
        field_bindings: Vec<AppSurfaceFieldBinding>,
        route_bindings: Vec<AppSurfaceRouteBinding>,
        surface_components: Vec<AppSurfaceComponentBinding>,
    ) -> Self {
        Self {
            entity: entity.clone(),
            view_kind,
            fields,
            field_bindings,
            route_bindings,
            surface_components,
            label_field: None,
            partition_field: None,
            parent_field: None,
            order_field: None,
            status_field: None,
            max_depth: None,
            timestamp_field: None,
            action_field: None,
            actor_field: None,
            type_field: None,
            target_field: None,
            default_actor: None,
            default_type: None,
        }
    }

    fn tree(
        entity: &AppName,
        fields: Vec<AppName>,
        field_bindings: Vec<AppSurfaceFieldBinding>,
        route_bindings: Vec<AppSurfaceRouteBinding>,
        label_field: AppName,
        view: &AppManifestView,
    ) -> Self {
        Self {
            entity: entity.clone(),
            view_kind: AppManifestViewKind::Tree,
            fields,
            field_bindings,
            route_bindings,
            surface_components: Vec::new(),
            label_field: Some(label_field),
            partition_field: view.partition_field.clone(),
            parent_field: view.parent_field.clone(),
            order_field: view.order_field.clone(),
            status_field: view.status_field.clone(),
            max_depth: Some(DEFAULT_TREE_MAX_DEPTH),
            timestamp_field: None,
            action_field: None,
            actor_field: None,
            type_field: None,
            target_field: None,
            default_actor: None,
            default_type: None,
        }
    }

    fn timeline(
        entity: &AppName,
        fields: Vec<AppName>,
        field_bindings: Vec<AppSurfaceFieldBinding>,
        route_bindings: Vec<AppSurfaceRouteBinding>,
        view: &AppManifestView,
        default_actor: String,
    ) -> Self {
        Self {
            entity: entity.clone(),
            view_kind: AppManifestViewKind::Timeline,
            fields,
            field_bindings,
            route_bindings,
            surface_components: Vec::new(),
            label_field: None,
            partition_field: None,
            parent_field: None,
            order_field: None,
            status_field: None,
            max_depth: None,
            timestamp_field: view.timestamp_field.clone(),
            action_field: view.action_field.clone(),
            actor_field: view.actor_field.clone(),
            type_field: view.type_field.clone(),
            target_field: view.target_field.clone(),
            default_actor: Some(default_actor),
            default_type: Some("update".to_owned()),
        }
    }

    fn graph(
        entity: &AppName,
        fields: Vec<AppName>,
        field_bindings: Vec<AppSurfaceFieldBinding>,
        route_bindings: Vec<AppSurfaceRouteBinding>,
        label_field: AppName,
        view: &AppManifestView,
    ) -> Self {
        Self {
            entity: entity.clone(),
            view_kind: AppManifestViewKind::Graph,
            fields,
            field_bindings,
            route_bindings,
            surface_components: Vec::new(),
            label_field: Some(label_field),
            partition_field: None,
            parent_field: view.parent_field.clone(),
            order_field: view.order_field.clone(),
            status_field: view.status_field.clone(),
            max_depth: None,
            timestamp_field: None,
            action_field: None,
            actor_field: None,
            type_field: None,
            target_field: None,
            default_actor: None,
            default_type: None,
        }
    }

    fn validate(&self) -> Result<(), AppSurfaceCompilerError> {
        if self.fields.is_empty() || self.fields.len() > MAX_SURFACE_FIELDS {
            return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                "viewBinding fields are empty or exceed the surface field ceiling".to_owned(),
            ));
        }
        let unique = self
            .fields
            .iter()
            .map(|field| field.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if unique.len() != self.fields.len() {
            return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                "viewBinding fields contain duplicates".to_owned(),
            ));
        }
        let unique_field_bindings = self
            .field_bindings
            .iter()
            .map(|binding| binding.field.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if self.field_bindings.is_empty()
            || self.field_bindings.len() > MAX_SURFACE_FIELDS
            || unique_field_bindings.len() != self.field_bindings.len()
            || self
                .fields
                .iter()
                .any(|field| !unique_field_bindings.contains(field.as_str()))
            || self.field_bindings.iter().any(|binding| {
                let unique_allowed_values = binding
                    .allowed_values
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>();
                (binding.kind == AppSurfaceFieldKind::Enum && binding.allowed_values.is_empty())
                    || (binding.kind != AppSurfaceFieldKind::Enum
                        && !binding.allowed_values.is_empty())
                    || unique_allowed_values.len() != binding.allowed_values.len()
                    || (binding.kind == AppSurfaceFieldKind::Reference
                        && binding.reference_entity.is_none())
                    || (binding.kind != AppSurfaceFieldKind::Reference
                        && binding.reference_entity.is_some())
            })
        {
            return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                "viewBinding field bindings are empty, duplicate or malformed".to_owned(),
            ));
        }
        let encoded_components = serde_json::to_vec(&self.surface_components)
            .map_err(|error| AppSurfaceCompilerError::InvalidQueryPlan(error.to_string()))?;
        if encoded_components.len() > APP_SURFACE_MAX_DECLARATIVE_BYTES {
            return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                "viewBinding surface components exceed the byte ceiling".to_owned(),
            ));
        }
        let mut component_ids = std::collections::BTreeSet::new();
        let mut component_count = 0usize;
        let mut component_stack = self
            .surface_components
            .iter()
            .rev()
            .map(|component| (component, 1usize))
            .collect::<Vec<_>>();
        while let Some((component, depth)) = component_stack.pop() {
            component_count = component_count.saturating_add(1);
            if component_count > APP_SURFACE_MAX_DECLARATIVE_COMPONENTS
                || depth > APP_SURFACE_MAX_DECLARATIVE_DEPTH
            {
                return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                    "viewBinding surface components exceed their graph ceiling".to_owned(),
                ));
            }
            let (id, label, fields, children): (
                &AppName,
                &str,
                Option<&[AppName]>,
                Option<&[AppSurfaceComponentBinding]>,
            ) = match component {
                AppSurfaceComponentBinding::Detail { id, label, fields }
                | AppSurfaceComponentBinding::Form { id, label, fields }
                | AppSurfaceComponentBinding::List { id, label, fields } => {
                    (id, label, Some(fields.as_slice()), None)
                },
                AppSurfaceComponentBinding::Table { id, label, columns } => {
                    (id, label, Some(columns.as_slice()), None)
                },
                AppSurfaceComponentBinding::Section {
                    id,
                    label,
                    children,
                } => (id, label, None, Some(children.as_slice())),
            };
            if !component_ids.insert(id.as_str()) || label != display_label(id.as_str()) {
                return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                    "viewBinding surface component identity or label is invalid".to_owned(),
                ));
            }
            if let Some(fields) = fields {
                let unique_fields = fields
                    .iter()
                    .map(|field| field.as_str())
                    .collect::<std::collections::BTreeSet<_>>();
                if fields.is_empty()
                    || fields.len() > MAX_SURFACE_FIELDS
                    || unique_fields.len() != fields.len()
                    || fields.iter().any(|field| {
                        !unique_field_bindings.contains(field.as_str())
                            || !self.fields.iter().any(|visible| visible == field)
                    })
                {
                    return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                        "viewBinding surface component fields are invalid".to_owned(),
                    ));
                }
            }
            if let Some(children) = children {
                if children.is_empty() {
                    return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                        "viewBinding surface section is empty".to_owned(),
                    ));
                }
                component_stack.extend(
                    children
                        .iter()
                        .rev()
                        .map(|child| (child, depth.saturating_add(1))),
                );
            }
        }
        if !self.surface_components.is_empty()
            && !matches!(
                self.view_kind,
                AppManifestViewKind::List | AppManifestViewKind::Table
            )
        {
            return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                "viewBinding surface components require collection query semantics".to_owned(),
            ));
        }
        if self.route_bindings.len() > 16 {
            return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                "viewBinding route bindings exceed the route segment ceiling".to_owned(),
            ));
        }
        let mut parameters = std::collections::BTreeSet::new();
        let mut route_fields = std::collections::BTreeSet::new();
        for binding in &self.route_bindings {
            let allowed = binding
                .allowed_values
                .iter()
                .collect::<std::collections::BTreeSet<_>>();
            if !parameters.insert(binding.parameter.as_str())
                || !route_fields.insert(binding.field.as_str())
                || binding.field != binding.parameter
                || (binding.scalar_kind == AppSurfaceRouteScalarKind::Enum
                    && binding.allowed_values.is_empty())
                || (binding.scalar_kind != AppSurfaceRouteScalarKind::Enum
                    && !binding.allowed_values.is_empty())
                || allowed.len() != binding.allowed_values.len()
            {
                return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                    "viewBinding route bindings are duplicate or have an invalid scalar shape"
                        .to_owned(),
                ));
            }
        }
        let contains = |field: &Option<AppName>| {
            field
                .as_ref()
                .is_none_or(|field| self.fields.iter().any(|candidate| candidate == field))
        };
        if !contains(&self.label_field)
            || !contains(&self.partition_field)
            || !contains(&self.parent_field)
            || !contains(&self.order_field)
            || !contains(&self.status_field)
            || !contains(&self.timestamp_field)
            || !contains(&self.action_field)
            || !contains(&self.actor_field)
            || !contains(&self.type_field)
            || !contains(&self.target_field)
        {
            return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                "viewBinding metadata names a field outside its projection".to_owned(),
            ));
        }
        let valid_shape = match self.view_kind {
            AppManifestViewKind::List | AppManifestViewKind::Table => {
                self.label_field.is_none()
                    && self.partition_field.is_none()
                    && self.parent_field.is_none()
                    && self.order_field.is_none()
                    && self.status_field.is_none()
                    && self.max_depth.is_none()
                    && self.timestamp_field.is_none()
                    && self.action_field.is_none()
                    && self.actor_field.is_none()
                    && self.type_field.is_none()
                    && self.target_field.is_none()
                    && self.default_actor.is_none()
                    && self.default_type.is_none()
            },
            AppManifestViewKind::Tree => {
                self.label_field.is_some()
                    && self.max_depth == Some(DEFAULT_TREE_MAX_DEPTH)
                    && self.timestamp_field.is_none()
                    && self.action_field.is_none()
                    && self.actor_field.is_none()
                    && self.type_field.is_none()
                    && self.target_field.is_none()
                    && self.default_actor.is_none()
                    && self.default_type.is_none()
            },
            AppManifestViewKind::Timeline => {
                self.label_field.is_none()
                    && self.partition_field.is_none()
                    && self.parent_field.is_none()
                    && self.order_field.is_none()
                    && self.status_field.is_none()
                    && self.max_depth.is_none()
                    && self.timestamp_field.is_some()
                    && self.action_field.is_some()
                    && self
                        .default_actor
                        .as_ref()
                        .is_some_and(|value| !value.is_empty())
                    && self.default_type.as_deref() == Some("update")
            },
            AppManifestViewKind::Graph => {
                self.label_field.is_some()
                    && self.partition_field.is_none()
                    && self.parent_field.is_some()
                    && self.max_depth.is_none()
                    && self.timestamp_field.is_none()
                    && self.action_field.is_none()
                    && self.actor_field.is_none()
                    && self.type_field.is_none()
                    && self.target_field.is_none()
                    && self.default_actor.is_none()
                    && self.default_type.is_none()
            },
            AppManifestViewKind::Board => false,
        };
        if !valid_shape {
            return Err(AppSurfaceCompilerError::InvalidQueryPlan(
                "viewBinding shape does not match its view kind".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Server-owned result of compiling one view at one exact schema revision.
/// The durable binding and render envelope are produced from the same inputs,
/// so callers cannot accidentally persist a digest for a different document.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledAppSurface {
    binding: AppSurfaceBinding,
    envelope: AppSurfaceEnvelope,
}

impl CompiledAppSurface {
    pub fn binding(&self) -> &AppSurfaceBinding {
        &self.binding
    }

    pub fn envelope(&self) -> &AppSurfaceEnvelope {
        &self.envelope
    }

    pub fn into_parts(self) -> (AppSurfaceBinding, AppSurfaceEnvelope) {
        (self.binding, self.envelope)
    }
}

/// One active surface generation containing every route declared by the
/// admitted package. `surface_revision` identifies this complete set; each
/// member remains an independently addressable `AppSurfaceBinding`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledAppSurfaceSet {
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    surface_revision: AppRevision,
    compiled_set_digest: AppDigest,
    surfaces: BTreeMap<AppName, CompiledAppSurface>,
}

impl CompiledAppSurfaceSet {
    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub fn surface_revision(&self) -> AppRevision {
        self.surface_revision
    }

    pub fn package_revision_ref(&self) -> &AppReference {
        &self.package_revision_ref
    }

    pub fn schema_revision(&self) -> AppRevision {
        self.schema_revision
    }

    pub fn compiled_set_digest(&self) -> &AppDigest {
        &self.compiled_set_digest
    }

    pub fn surface(&self, view_id: &AppName) -> Option<&CompiledAppSurface> {
        self.surfaces.get(view_id)
    }

    pub fn surfaces(&self) -> &BTreeMap<AppName, CompiledAppSurface> {
        &self.surfaces
    }

    pub fn into_surfaces(self) -> BTreeMap<AppName, CompiledAppSurface> {
        self.surfaces
    }
}

#[derive(Debug, Error)]
pub enum AppSurfaceCompilerError {
    #[error("app view `{0}` does not exist in the admitted package")]
    ViewNotFound(String),
    #[error("app view `{view}` uses unsupported V1 view kind `{kind}`")]
    UnsupportedViewKind { view: String, kind: &'static str },
    #[error("app view `{view}` exposes {actual} fields; the V1 surface ceiling is {limit}")]
    SurfaceFieldLimit {
        view: String,
        actual: usize,
        limit: usize,
    },
    #[error("tree view `{view}` has no non-structural text field for its node label")]
    MissingTreeLabel { view: String },
    #[error("graph view `{view}` has no non-structural text field for its node label")]
    MissingGraphNodeLabel { view: String },
    #[error("failed to encode the canonical app surface: {0}")]
    CanonicalEncoding(String),
    #[error("app surface entity schema does not match the immutable package revision")]
    EntitySchemaDigestMismatch,
    #[error("app surface view schema does not match the immutable package revision")]
    ViewSchemaDigestMismatch,
    #[error("app surface package evidence is inconsistent: {0}")]
    PackageEvidenceMismatch(&'static str),
    #[error("app surface schema evidence is inconsistent: {0}")]
    SchemaEvidenceMismatch(&'static str),
    #[error("persisted app surface envelope is invalid: {0}")]
    PersistedEnvelope(String),
    #[error("compiled app surface query plan is invalid: {0}")]
    InvalidQueryPlan(String),
    #[error("persisted app surface member does not match its compiled digest")]
    CompiledMemberDigestMismatch,
    #[error("persisted app surface generation does not match its compiled digest")]
    CompiledSetDigestMismatch,
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Muij(#[from] MuijValidationError),
    #[error(transparent)]
    Schema(#[from] AppSchemaCompilerError),
}

#[derive(Serialize)]
struct CompiledViewDigestMaterial<'a> {
    compiler_version: u32,
    installation_id: &'a AppInstallationId,
    package_revision_ref: &'a AppReference,
    schema_revision: AppRevision,
    view_id: &'a AppName,
    app_local_route: &'a str,
    muij_version: &'a str,
    document_owner_id: &'a str,
    layout: &'a [MuijComponent],
}

#[derive(Serialize)]
struct CompiledSurfaceSetDigestMaterial<'a> {
    compiler_version: u32,
    installation_id: &'a AppInstallationId,
    package_revision_ref: &'a AppReference,
    schema_revision: AppRevision,
    surface_revision: AppRevision,
    members: Vec<CompiledSurfaceSetDigestMember<'a>>,
}

#[derive(Serialize)]
struct CompiledSurfaceSetDigestMember<'a> {
    view_id: &'a AppName,
    app_local_route: &'a str,
    compiled_view_digest: &'a AppDigest,
}

/// Compile one admitted manifest view for authoring preview or diagnostics.
///
/// Preview returns only the serialization envelope. It cannot mint or expose an
/// active durable binding; activation must use [`compile_app_surface_set`].
pub fn compile_app_surface_preview(
    manifest: &CanonicalAppManifest,
    installation_id: AppInstallationId,
    schema_revision: AppRevision,
    surface_revision: AppRevision,
    view_id: &AppName,
    compiled_at: DateTime<Utc>,
) -> Result<AppSurfaceEnvelope, AppSurfaceCompilerError> {
    let preview_ref = AppReference::parse(format!(
        "surface-preview:{}",
        manifest.manifest_digest().as_str()
    ))?;
    let compiled = compile_verified_app_surface(
        manifest.manifest(),
        installation_id,
        preview_ref,
        schema_revision,
        surface_revision,
        view_id,
        compiled_at,
    )?;
    Ok(compiled.envelope)
}

/// Compile every declared view in one ordered pass after verifying immutable
/// package schema identity once. This is the production path for activation;
/// it avoids O(view_count × schema_size) digest work and prevents a multi-route
/// app from being reduced to whichever view happened to compile first.
pub fn compile_app_surface_set(
    source: &VerifiedAppSurfaceSource,
    surface_revision: AppRevision,
) -> Result<CompiledAppSurfaceSet, AppSurfaceCompilerError> {
    let manifest = source.manifest.manifest();
    validate_manifest_schema_identity(
        manifest,
        &source.entity_schema_digest,
        &source.view_schema_digest,
    )?;

    let mut surfaces = BTreeMap::new();
    for view_id in manifest.app.views.keys() {
        let compiled = compile_verified_app_surface(
            manifest,
            source.installation_id.clone(),
            source.package_revision_ref.clone(),
            source.schema_revision,
            surface_revision,
            view_id,
            source.schema_created_at,
        )?;
        surfaces.insert(view_id.clone(), compiled);
    }

    let compiled_set_digest = compiled_surface_set_digest(
        &source.installation_id,
        &source.package_revision_ref,
        source.schema_revision,
        surface_revision,
        surfaces.values().map(|surface| {
            (
                &surface.binding().view_id,
                surface.binding().app_local_route.as_str(),
                &surface.binding().compiled_view_digest,
            )
        }),
    )?;

    Ok(CompiledAppSurfaceSet {
        installation_id: source.installation_id.clone(),
        package_revision_ref: source.package_revision_ref.clone(),
        schema_revision: source.schema_revision,
        surface_revision,
        compiled_set_digest,
        surfaces,
    })
}

fn validate_manifest_schema_identity(
    manifest: &AppPackageManifest,
    expected_entity_schema_digest: &AppDigest,
    expected_view_schema_digest: &AppDigest,
) -> Result<(), AppSurfaceCompilerError> {
    if &canonical_entity_schema_digest(manifest)? != expected_entity_schema_digest {
        return Err(AppSurfaceCompilerError::EntitySchemaDigestMismatch);
    }
    let view_schema_digest = canonical_view_schema_digest(manifest)
        .map_err(|error| AppSurfaceCompilerError::CanonicalEncoding(error.to_string()))?;
    if &view_schema_digest != expected_view_schema_digest {
        return Err(AppSurfaceCompilerError::ViewSchemaDigestMismatch);
    }
    Ok(())
}

fn compiled_view_digest(
    installation_id: &AppInstallationId,
    package_revision_ref: &AppReference,
    schema_revision: AppRevision,
    view_id: &AppName,
    app_local_route: &str,
    document: &MuijDocument,
) -> Result<AppDigest, AppSurfaceCompilerError> {
    let digest_material = CompiledViewDigestMaterial {
        compiler_version: APP_SURFACE_COMPILER_VERSION,
        installation_id,
        package_revision_ref,
        schema_revision,
        view_id,
        app_local_route,
        muij_version: &document.muij_version,
        document_owner_id: &document.agent_id,
        layout: &document.layout,
    };
    let digest_value = serde_json::to_value(digest_material)
        .map_err(|error| AppSurfaceCompilerError::CanonicalEncoding(error.to_string()))?;
    let digest_bytes = canonical_json_bytes(&digest_value)
        .map_err(|error| AppSurfaceCompilerError::CanonicalEncoding(error.to_string()))?;
    Ok(AppDigest::blake3(&digest_bytes))
}

pub fn compiled_surface_set_digest<'a>(
    installation_id: &AppInstallationId,
    package_revision_ref: &AppReference,
    schema_revision: AppRevision,
    surface_revision: AppRevision,
    bindings: impl IntoIterator<Item = (&'a AppName, &'a str, &'a AppDigest)>,
) -> Result<AppDigest, AppSurfaceCompilerError> {
    let mut ordered = bindings.into_iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.0.cmp(right.0));
    let members = ordered
        .into_iter()
        .map(
            |(view_id, app_local_route, compiled_view_digest)| CompiledSurfaceSetDigestMember {
                view_id,
                app_local_route,
                compiled_view_digest,
            },
        )
        .collect();
    let digest_material = CompiledSurfaceSetDigestMaterial {
        compiler_version: APP_SURFACE_COMPILER_VERSION,
        installation_id,
        package_revision_ref,
        schema_revision,
        surface_revision,
        members,
    };
    let digest_value = serde_json::to_value(digest_material)
        .map_err(|error| AppSurfaceCompilerError::CanonicalEncoding(error.to_string()))?;
    let digest_bytes = canonical_json_bytes(&digest_value)
        .map_err(|error| AppSurfaceCompilerError::CanonicalEncoding(error.to_string()))?;
    Ok(AppDigest::blake3(&digest_bytes))
}

pub fn verify_persisted_surface_member(
    binding: &AppSurfaceBinding,
    envelope: &AppSurfaceEnvelope,
    package_revision_ref: &AppReference,
    schema_revision: AppRevision,
    schema_created_at: DateTime<Utc>,
) -> Result<AppSurfaceQueryPlan, AppSurfaceCompilerError> {
    binding.validate_app_contract(&AppContractLimits::default())?;
    envelope.muij_document.validate(&DefaultComponentRegistry)?;
    let expected_published_surface_ref =
        app_published_surface_ref(&binding.installation_id, &binding.view_id)?;
    if binding.installation_id != envelope.installation_id
        || binding.surface_revision != envelope.surface_revision
        || binding.view_id != envelope.view_id
        || &binding.package_revision_ref != package_revision_ref
        || envelope.muij_document.generated_at != schema_created_at
        || binding.status != AppSurfaceStatus::Active
        || binding.published_surface_ref.as_ref() != Some(&expected_published_surface_ref)
    {
        return Err(AppSurfaceCompilerError::CompiledMemberDigestMismatch);
    }
    let digest = compiled_view_digest(
        &binding.installation_id,
        package_revision_ref,
        schema_revision,
        &binding.view_id,
        &binding.app_local_route,
        &envelope.muij_document,
    )?;
    if digest != binding.compiled_view_digest {
        return Err(AppSurfaceCompilerError::CompiledMemberDigestMismatch);
    }
    let query_plan = envelope.query_plan()?;
    query_plan.validate_route_template(&binding.app_local_route)?;
    Ok(query_plan)
}

#[allow(clippy::too_many_arguments)]
fn compile_verified_app_surface(
    manifest: &AppPackageManifest,
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    surface_revision: AppRevision,
    view_id: &AppName,
    compiled_at: DateTime<Utc>,
) -> Result<CompiledAppSurface, AppSurfaceCompilerError> {
    let view = manifest
        .app
        .views
        .get(view_id)
        .ok_or_else(|| AppSurfaceCompilerError::ViewNotFound(view_id.to_string()))?;
    if view.kind == AppManifestViewKind::Board {
        return Err(AppSurfaceCompilerError::UnsupportedViewKind {
            view: view_id.to_string(),
            kind: "board",
        });
    }
    let entity = manifest.app.entities.get(&view.entity).ok_or_else(|| {
        AppSurfaceCompilerError::CanonicalEncoding(
            "admitted view references a missing entity".to_owned(),
        )
    })?;

    let document_owner_id =
        component_id(&installation_id, view_id, schema_revision, "document_owner");
    let data_component = compile_data_component(
        &installation_id,
        schema_revision,
        &manifest.name,
        view_id,
        view,
        entity,
    )?;
    let view_label = display_label(view_id.as_str());
    let root = MuijComponent {
        id: component_id(&installation_id, view_id, schema_revision, "root"),
        component_type: "Stack".to_owned(),
        label: view_label.clone(),
        source: None,
        query: None,
        props: json!({ "gap": "var(--space-md)" }),
        static_snapshot: None,
        children: vec![
            MuijComponent {
                id: component_id(&installation_id, view_id, schema_revision, "title"),
                component_type: "Text".to_owned(),
                label: view_label.clone(),
                source: None,
                query: None,
                props: json!({ "children": view_label, "variant": "title" }),
                static_snapshot: None,
                children: Vec::new(),
            },
            data_component,
        ],
    };
    let muij_document = MuijDocument {
        muij_version: "1.0".to_owned(),
        agent_id: document_owner_id,
        layout: vec![root],
        generated_at: compiled_at,
    };
    muij_document.validate(&DefaultComponentRegistry)?;

    let compiled_view_digest = compiled_view_digest(
        &installation_id,
        &package_revision_ref,
        schema_revision,
        view_id,
        view.route.as_str(),
        &muij_document,
    )?;
    let binding = AppSurfaceBinding {
        installation_id: installation_id.clone(),
        surface_revision,
        package_revision_ref,
        app_local_route: view.route.as_str().to_owned(),
        canonical_host_route: canonical_host_route(&installation_id, view.route.as_str()),
        view_id: view_id.clone(),
        compiled_view_digest,
        published_surface_ref: Some(app_published_surface_ref(&installation_id, view_id)?),
        status: AppSurfaceStatus::Active,
    };
    binding.validate_app_contract(&AppContractLimits::default())?;

    Ok(CompiledAppSurface {
        envelope: AppSurfaceEnvelope {
            installation_id,
            surface_revision,
            view_id: view_id.clone(),
            muij_document,
            interaction_mode: AppSurfaceInteractionMode::App,
        },
        binding,
    })
}

pub fn app_published_surface_ref(
    installation_id: &AppInstallationId,
    view_id: &AppName,
) -> Result<AppReference, AppContractError> {
    // Installation and view identifiers can each be at their independent
    // contract ceiling, while AppReference has its own smaller total ceiling.
    // Hash length-prefixed identity material so every legal pair has a stable,
    // bounded publication identity without truncation collisions.
    let installation_bytes = installation_id.as_str().as_bytes();
    let view_bytes = view_id.as_str().as_bytes();
    let mut identity = Vec::with_capacity(
        std::mem::size_of::<u64>() * 2 + installation_bytes.len() + view_bytes.len(),
    );
    identity.extend_from_slice(&(installation_bytes.len() as u64).to_be_bytes());
    identity.extend_from_slice(installation_bytes);
    identity.extend_from_slice(&(view_bytes.len() as u64).to_be_bytes());
    identity.extend_from_slice(view_bytes);
    let digest = AppDigest::blake3(&identity);
    AppReference::parse(format!("app-surface:{}", digest.as_str()))
}

fn compile_data_component(
    installation_id: &AppInstallationId,
    schema_revision: AppRevision,
    app_name: &AppName,
    view_id: &AppName,
    view: &AppManifestView,
    entity: &AppManifestEntity,
) -> Result<MuijComponent, AppSurfaceCompilerError> {
    let id = component_id(installation_id, view_id, schema_revision, "data");
    let label = display_label(view_id.as_str());
    let query = Some(format!("view:{}", view_id.as_str()));
    let source = Some(APP_QUERY_SOURCE.to_owned());
    let route_bindings = compile_route_bindings(view, entity)?;
    let field_bindings = compile_field_bindings(entity);

    if !view.components.is_empty() {
        ensure_surface_field_limit(view_id, entity.fields.len())?;
        let surface_components = compile_surface_components(&view.components);
        let mut exposed = std::collections::BTreeSet::new();
        let mut stack = view.components.iter().collect::<Vec<_>>();
        while let Some(component) = stack.pop() {
            match component {
                AppManifestSurfaceComponent::Detail { fields, .. }
                | AppManifestSurfaceComponent::Form { fields, .. }
                | AppManifestSurfaceComponent::List { fields, .. } => {
                    exposed.extend(fields.iter().map(|field| field.as_str().to_owned()));
                },
                AppManifestSurfaceComponent::Table { columns, .. } => {
                    exposed.extend(columns.iter().map(|field| field.as_str().to_owned()));
                },
                AppManifestSurfaceComponent::Section { children, .. } => {
                    stack.extend(children.iter());
                },
            }
        }
        let projected_fields = entity
            .fields
            .keys()
            .filter(|field| exposed.contains(field.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        ensure_surface_field_limit(view_id, projected_fields.len())?;
        let view_binding = AppSurfaceViewBinding::collection(
            &view.entity,
            view.kind,
            projected_fields,
            field_bindings,
            route_bindings,
            surface_components,
        );
        view_binding.validate()?;
        return Ok(MuijComponent {
            id,
            component_type: "Stack".to_owned(),
            label,
            source,
            query,
            props: json!({
                "gap": "var(--space-md)",
                "viewBinding": view_binding,
            }),
            static_snapshot: None,
            children: Vec::new(),
        });
    }

    let (component_type, props) = match view.kind {
        AppManifestViewKind::List => {
            ensure_surface_field_limit(view_id, entity.fields.len())?;
            let projected_fields = entity.fields.keys().cloned().collect::<Vec<_>>();
            let columns = entity
                .fields
                .iter()
                .map(|(field, _)| column_props(field, false))
                .collect::<Vec<_>>();
            let view_binding = AppSurfaceViewBinding::collection(
                &view.entity,
                AppManifestViewKind::List,
                projected_fields,
                field_bindings,
                route_bindings,
                Vec::new(),
            );
            (
                "EntityGrid",
                json!({
                    "columns": columns,
                    "rows": [],
                    "pageSize": DEFAULT_SURFACE_PAGE_SIZE,
                    "paginationMode": "server",
                    "currentPage": 1,
                    "pageCount": 1,
                    "totalItems": 0,
                    "startItem": 0,
                    "endItem": 0,
                    "rowIdKey": "__record_id",
                    "expandable": false,
                    "viewBinding": view_binding,
                }),
            )
        },
        AppManifestViewKind::Table => {
            ensure_surface_field_limit(view_id, view.columns.len())?;
            let projected_fields = view.columns.clone();
            let columns = view
                .columns
                .iter()
                .map(|field| {
                    let sortable = entity.fields.get(field).is_some_and(field_supports_sort);
                    column_props(field, sortable)
                })
                .collect::<Vec<_>>();
            let view_binding = AppSurfaceViewBinding::collection(
                &view.entity,
                AppManifestViewKind::Table,
                projected_fields,
                field_bindings,
                route_bindings,
                Vec::new(),
            );
            (
                "EntityGrid",
                json!({
                    "columns": columns,
                    "rows": [],
                    "pageSize": DEFAULT_SURFACE_PAGE_SIZE,
                    "paginationMode": "server",
                    "currentPage": 1,
                    "pageCount": 1,
                    "totalItems": 0,
                    "startItem": 0,
                    "endItem": 0,
                    "rowIdKey": "__record_id",
                    "expandable": false,
                    "viewBinding": view_binding,
                }),
            )
        },
        AppManifestViewKind::Tree => {
            let label_field = tree_display_field(view, entity).ok_or_else(|| {
                AppSurfaceCompilerError::MissingTreeLabel {
                    view: view_id.to_string(),
                }
            })?;
            let projected_fields =
                unique_surface_fields(std::iter::once(Some(label_field)).chain([
                    view.partition_field.as_ref(),
                    view.parent_field.as_ref(),
                    view.order_field.as_ref(),
                    view.status_field.as_ref(),
                ]));
            let view_binding = AppSurfaceViewBinding::tree(
                &view.entity,
                projected_fields,
                field_bindings,
                route_bindings,
                label_field.clone(),
                view,
            );
            (
                "Tree",
                json!({
                    "nodes": [],
                    "expandAll": false,
                    "maxDepth": DEFAULT_TREE_MAX_DEPTH,
                    "viewBinding": view_binding,
                }),
            )
        },
        AppManifestViewKind::Timeline => {
            let projected_fields = unique_surface_fields([
                view.timestamp_field.as_ref(),
                view.action_field.as_ref(),
                view.actor_field.as_ref(),
                view.type_field.as_ref(),
                view.target_field.as_ref(),
            ]);
            let view_binding = AppSurfaceViewBinding::timeline(
                &view.entity,
                projected_fields,
                field_bindings,
                route_bindings,
                view,
                display_label(app_name.as_str()),
            );
            (
                "ActivityFeed",
                json!({
                    "items": [],
                    "maxItems": DEFAULT_SURFACE_PAGE_SIZE,
                    "viewBinding": view_binding,
                }),
            )
        },
        AppManifestViewKind::Graph => {
            let label_field = tree_display_field(view, entity).ok_or_else(|| {
                AppSurfaceCompilerError::MissingGraphNodeLabel {
                    view: view_id.to_string(),
                }
            })?;
            let projected_fields =
                unique_surface_fields(std::iter::once(Some(label_field)).chain([
                    view.partition_field.as_ref(),
                    view.parent_field.as_ref(),
                    view.order_field.as_ref(),
                    view.status_field.as_ref(),
                ]));
            let view_binding = AppSurfaceViewBinding::graph(
                &view.entity,
                projected_fields,
                field_bindings,
                route_bindings,
                label_field.clone(),
                view,
            );
            // Empty skeleton in the MUIJ `Graph` family's exact prop shape
            // (nodes/edges/layout decode into `MuijGraphSpec`); live nodes
            // and parent→child edges are hydrated from the entity page the
            // viewBinding's field mapping names, exactly as every other
            // data component hydrates its rows.
            (
                "Graph",
                json!({
                    "nodes": [],
                    "edges": [],
                    "layout": "layered",
                    "viewBinding": view_binding,
                }),
            )
        },
        AppManifestViewKind::Board => {
            return Err(AppSurfaceCompilerError::UnsupportedViewKind {
                view: view_id.to_string(),
                kind: "board",
            });
        },
    };

    Ok(MuijComponent {
        id,
        component_type: component_type.to_owned(),
        label,
        source,
        query,
        props,
        static_snapshot: None,
        children: Vec::new(),
    })
}

fn compile_surface_components(
    components: &[AppManifestSurfaceComponent],
) -> Vec<AppSurfaceComponentBinding> {
    components
        .iter()
        .map(|component| match component {
            AppManifestSurfaceComponent::Detail { id, fields } => {
                AppSurfaceComponentBinding::Detail {
                    id: id.clone(),
                    label: display_label(id.as_str()),
                    fields: fields.clone(),
                }
            },
            AppManifestSurfaceComponent::Form { id, fields } => AppSurfaceComponentBinding::Form {
                id: id.clone(),
                label: display_label(id.as_str()),
                fields: fields.clone(),
            },
            AppManifestSurfaceComponent::Section { id, children } => {
                AppSurfaceComponentBinding::Section {
                    id: id.clone(),
                    label: display_label(id.as_str()),
                    children: compile_surface_components(children),
                }
            },
            AppManifestSurfaceComponent::List { id, fields } => AppSurfaceComponentBinding::List {
                id: id.clone(),
                label: display_label(id.as_str()),
                fields: fields.clone(),
            },
            AppManifestSurfaceComponent::Table { id, columns } => {
                AppSurfaceComponentBinding::Table {
                    id: id.clone(),
                    label: display_label(id.as_str()),
                    columns: columns.clone(),
                }
            },
        })
        .collect()
}

fn compile_route_bindings(
    view: &AppManifestView,
    entity: &AppManifestEntity,
) -> Result<Vec<AppSurfaceRouteBinding>, AppSurfaceCompilerError> {
    view.route
        .parameter_names()
        .map(|parameter| {
            let parameter = AppName::parse(parameter)?;
            let field = entity.fields.get(&parameter).ok_or_else(|| {
                AppSurfaceCompilerError::InvalidQueryPlan(format!(
                    "route parameter `{parameter}` has no entity field"
                ))
            })?;
            let (scalar_kind, allowed_values) = match field {
                AppManifestField::Text { .. } => (AppSurfaceRouteScalarKind::Text, Vec::new()),
                AppManifestField::Integer { .. } => {
                    (AppSurfaceRouteScalarKind::Integer, Vec::new())
                },
                AppManifestField::Decimal { .. } => {
                    (AppSurfaceRouteScalarKind::Decimal, Vec::new())
                },
                AppManifestField::Boolean { .. } => {
                    (AppSurfaceRouteScalarKind::Boolean, Vec::new())
                },
                AppManifestField::Enum { values, .. } => {
                    (AppSurfaceRouteScalarKind::Enum, values.clone())
                },
                AppManifestField::Reference { .. } => {
                    (AppSurfaceRouteScalarKind::Reference, Vec::new())
                },
                AppManifestField::Markdown { .. } | AppManifestField::Timestamp { .. } => {
                    return Err(AppSurfaceCompilerError::InvalidQueryPlan(format!(
                        "route parameter `{parameter}` is not a portable scalar field"
                    )));
                },
            };
            Ok(AppSurfaceRouteBinding {
                field: parameter.clone(),
                parameter,
                scalar_kind,
                allowed_values,
            })
        })
        .collect()
}

fn compile_field_bindings(entity: &AppManifestEntity) -> Vec<AppSurfaceFieldBinding> {
    entity
        .fields
        .iter()
        .map(|(field, contract)| {
            let (kind, required, nullable, allowed_values, reference_entity) = match contract {
                AppManifestField::Text {
                    required, nullable, ..
                } => (
                    AppSurfaceFieldKind::Text,
                    *required,
                    *nullable,
                    Vec::new(),
                    None,
                ),
                AppManifestField::Markdown {
                    required, nullable, ..
                } => (
                    AppSurfaceFieldKind::Markdown,
                    *required,
                    *nullable,
                    Vec::new(),
                    None,
                ),
                AppManifestField::Integer {
                    required, nullable, ..
                } => (
                    AppSurfaceFieldKind::Integer,
                    *required,
                    *nullable,
                    Vec::new(),
                    None,
                ),
                AppManifestField::Decimal {
                    required, nullable, ..
                } => (
                    AppSurfaceFieldKind::Decimal,
                    *required,
                    *nullable,
                    Vec::new(),
                    None,
                ),
                AppManifestField::Boolean {
                    required, nullable, ..
                } => (
                    AppSurfaceFieldKind::Boolean,
                    *required,
                    *nullable,
                    Vec::new(),
                    None,
                ),
                AppManifestField::Timestamp {
                    required, nullable, ..
                } => (
                    AppSurfaceFieldKind::Timestamp,
                    *required,
                    *nullable,
                    Vec::new(),
                    None,
                ),
                AppManifestField::Enum {
                    values,
                    required,
                    nullable,
                    ..
                } => (
                    AppSurfaceFieldKind::Enum,
                    *required,
                    *nullable,
                    values.clone(),
                    None,
                ),
                AppManifestField::Reference {
                    entity,
                    required,
                    nullable,
                    ..
                } => (
                    AppSurfaceFieldKind::Reference,
                    *required,
                    *nullable,
                    Vec::new(),
                    Some(entity.clone()),
                ),
            };
            AppSurfaceFieldBinding {
                field: field.clone(),
                kind,
                required,
                nullable,
                sortable: field_supports_sort(contract),
                allowed_values,
                reference_entity,
            }
        })
        .collect()
}

fn ensure_surface_field_limit(
    view_id: &AppName,
    field_count: usize,
) -> Result<(), AppSurfaceCompilerError> {
    if field_count > MAX_SURFACE_FIELDS {
        return Err(AppSurfaceCompilerError::SurfaceFieldLimit {
            view: view_id.to_string(),
            actual: field_count,
            limit: MAX_SURFACE_FIELDS,
        });
    }
    Ok(())
}

fn unique_surface_fields<'a>(
    fields: impl IntoIterator<Item = Option<&'a AppName>>,
) -> Vec<AppName> {
    let mut seen = std::collections::BTreeSet::new();
    fields
        .into_iter()
        .flatten()
        .filter(|field| seen.insert(field.as_str().to_owned()))
        .cloned()
        .collect()
}

fn column_props(field: &AppName, sortable: bool) -> Value {
    json!({
        "key": field.as_str(),
        "label": display_label(field.as_str()),
        "sortable": sortable,
    })
}

fn field_supports_sort(field: &AppManifestField) -> bool {
    !matches!(field, AppManifestField::Markdown { .. })
}

fn canonical_host_route(installation_id: &AppInstallationId, app_local_route: &str) -> String {
    if app_local_route == "/" {
        format!("/apps/{}", installation_id.as_str())
    } else {
        format!("/apps/{}{}", installation_id.as_str(), app_local_route)
    }
}

fn component_id(
    installation_id: &AppInstallationId,
    view_id: &AppName,
    schema_revision: AppRevision,
    logical_path: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_part(&mut hasher, installation_id.as_str().as_bytes());
    hash_part(&mut hasher, view_id.as_str().as_bytes());
    hash_part(&mut hasher, &schema_revision.get().to_be_bytes());
    hash_part(&mut hasher, logical_path.as_bytes());
    format!("app-{}", hasher.finalize().to_hex())
}

fn hash_part(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn display_label(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut capitalize = true;
    for byte in value.bytes() {
        if matches!(byte, b'_' | b'-') {
            if !output.ends_with(' ') {
                output.push(' ');
            }
            capitalize = true;
        } else if capitalize {
            output.push((byte as char).to_ascii_uppercase());
            capitalize = false;
        } else {
            output.push(byte as char);
        }
    }
    output
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::magician_v2::{
        apps::{
            manifest::{
                build_app_package_candidate, parse_app_manifest_frontmatter,
                tests::{valid_bundle, valid_skill_document},
                AppPackageLimits,
            },
            models::{AppDataClassification, AppModelProcessing},
            package_staging::materialize_test_staged_package,
            records::{
                AppCompatibilityRequirement, AppDataHandlingPolicy, AppExternalEgress,
                AppMemoryPromotion, AppPackageSourceKind, AppPersonalAgentAccess,
                AppSchemaCompatibility, AppScope,
            },
            registry::tests::{canonical_tempdir, reference},
            schema_compiler::compile_app_schema,
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 17, 0, 0, second)
            .single()
            .unwrap()
    }

    fn compile_result(
        document: &str,
        view: &str,
        at: DateTime<Utc>,
    ) -> Result<CompiledAppSurface, AppSurfaceCompilerError> {
        let manifest =
            parse_app_manifest_frontmatter(document.as_bytes(), &AppPackageLimits::default())
                .unwrap();
        let entity_digest = canonical_entity_schema_digest(manifest.manifest()).unwrap();
        let view_digest = canonical_view_schema_digest(manifest.manifest()).unwrap();
        let source = VerifiedAppSurfaceSource::for_test(
            manifest,
            AppInstallationId::parse("install_surface_1").unwrap(),
            AppReference::parse("package:surface:1").unwrap(),
            AppRevision::new(7).unwrap(),
            at,
            entity_digest,
            view_digest,
        );
        let view_id = AppName::parse(view).unwrap();
        let compiled = compile_app_surface_set(&source, AppRevision::new(3).unwrap())?;
        compiled
            .surface(&view_id)
            .cloned()
            .ok_or_else(|| AppSurfaceCompilerError::ViewNotFound(view.to_owned()))
    }

    fn compile(document: &str, view: &str, at: DateTime<Utc>) -> CompiledAppSurface {
        compile_result(document, view, at).unwrap()
    }

    fn compile_set(document: &str, at: DateTime<Utc>) -> CompiledAppSurfaceSet {
        let manifest =
            parse_app_manifest_frontmatter(document.as_bytes(), &AppPackageLimits::default())
                .unwrap();
        let entity_digest = canonical_entity_schema_digest(manifest.manifest()).unwrap();
        let view_digest = canonical_view_schema_digest(manifest.manifest()).unwrap();
        let source = VerifiedAppSurfaceSource::for_test(
            manifest,
            AppInstallationId::parse("install_surface_1").unwrap(),
            AppReference::parse("package:surface:1").unwrap(),
            AppRevision::new(7).unwrap(),
            at,
            entity_digest,
            view_digest,
        );
        compile_app_surface_set(&source, AppRevision::new(3).unwrap()).unwrap()
    }

    fn data_component(surface: &CompiledAppSurface) -> &MuijComponent {
        &surface.envelope().muij_document().layout[0].children[1]
    }

    #[test]
    fn registry_snapshot_evidence_is_required_for_production_compilation() {
        let temporary = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(temporary.path());
        let candidate =
            build_app_package_candidate(valid_bundle(), &AppPackageLimits::default()).unwrap();
        let manifest = candidate.manifest().manifest();
        let entity_digest = canonical_entity_schema_digest(manifest).unwrap();
        let package_revision = AppPackageRevision {
            package_id: reference("app:surface-evidence"),
            semantic_version: manifest.version.clone(),
            content_digest: candidate.bundle_digest().clone(),
            manifest_schema_version: manifest.metadata.magician.app_manifest_version.clone(),
            authoring_sdk_version: manifest.metadata.magician.app_sdk_version.clone(),
            publisher_identity: reference("publisher:owner"),
            source_kind: AppPackageSourceKind::LocalVibedev,
            compatibility: manifest
                .app
                .compatibility
                .iter()
                .map(|(contract, requirement)| AppCompatibilityRequirement {
                    contract: contract.clone(),
                    requirement: requirement.clone(),
                })
                .collect(),
            requested_authority_digest: AppDigest::blake3(b"authority"),
            requested_data_policy_digest: AppDigest::blake3(b"policy"),
            dependency_lock_digest: AppDigest::blake3(b"lock"),
            entity_schema_digest: entity_digest.clone(),
            view_schema_digest: canonical_view_schema_digest(manifest).unwrap(),
            workflow_digest: AppDigest::blake3(b"workflow"),
            verification_attestation_ref: None,
            conformance_attestation_ref: reference("attestation:conformance"),
            created_at: time(1),
        };
        let package_ref = canonical_package_revision_ref(&package_revision).unwrap();
        let installation = AppInstallationId::parse("install_surface_evidence").unwrap();
        let policy = AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: AppModelProcessing::None,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        };
        let schema = compile_app_schema(
            manifest,
            installation.clone(),
            package_ref.clone(),
            AppRevision::new(1).unwrap(),
            policy,
            AppSchemaCompatibility::Initial,
            None,
            &entity_digest,
            time(2),
        )
        .unwrap()
        .into_revision();
        let staged = materialize_test_staged_package(
            &workspace,
            &AppScope {
                principal: reference("anonymous"),
                workspace: reference("default"),
            },
            candidate,
        );

        let source =
            VerifiedAppSurfaceSource::from_registry_snapshot(&staged, &package_revision, &schema)
                .unwrap();
        let compiled = compile_app_surface_set(&source, AppRevision::new(1).unwrap()).unwrap();
        assert_eq!(compiled.installation_id(), &installation);
        assert_eq!(compiled.package_revision_ref(), &package_ref);
        assert_eq!(compiled.schema_revision(), schema.revision);
    }

    #[test]
    fn list_surface_is_empty_bounded_and_carries_no_record_snapshot() {
        let surface = compile(&valid_skill_document(), "plans", time(1));
        let data = data_component(&surface);
        assert_eq!(data.component_type, "EntityGrid");
        assert_eq!(data.source.as_deref(), Some(APP_QUERY_SOURCE));
        assert_eq!(data.query.as_deref(), Some("view:plans"));
        assert_eq!(data.props["rows"], json!([]));
        assert_eq!(data.props["pageSize"], DEFAULT_SURFACE_PAGE_SIZE);
        assert_eq!(data.props["paginationMode"], "server");
        assert_eq!(data.props["currentPage"], 1);
        assert_eq!(data.props["totalItems"], 0);
        assert_eq!(data.props["columns"][0]["key"], "status");
        assert_eq!(data.props["columns"][1]["key"], "topic");
        assert_eq!(
            data.props["viewBinding"]["fields"],
            json!(["status", "topic"])
        );
        assert!(data.props["viewBinding"].get("surfaceComponents").is_none());
        assert_eq!(
            data.props["viewBinding"]["fieldBindings"],
            json!([
                {
                    "field": "status",
                    "kind": "enum",
                    "required": true,
                    "nullable": false,
                    "sortable": true,
                    "allowedValues": ["new", "done"]
                },
                {
                    "field": "topic",
                    "kind": "text",
                    "required": true,
                    "nullable": false,
                    "sortable": true
                }
            ])
        );
        assert!(data.static_snapshot.is_none());
        assert_eq!(
            surface.binding().canonical_host_route,
            "/apps/install_surface_1"
        );
        assert_eq!(
            surface.envelope().interaction_mode(),
            AppSurfaceInteractionMode::App
        );
    }

    #[test]
    fn declarative_surface_compiles_one_server_owned_bounded_query_binding() {
        let document = valid_skill_document().replace(
            "      route: /\n",
            r#"      route: /
      components:
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
          fields: [topic]
        - component: table
          id: plan_table
          columns: [status, topic]
"#,
        );
        let surface = compile(&document, "plans", time(1));
        let data = data_component(&surface);
        assert_eq!(data.component_type, "Stack");
        assert_eq!(data.source.as_deref(), Some(APP_QUERY_SOURCE));
        assert_eq!(data.query.as_deref(), Some("view:plans"));
        assert!(data.static_snapshot.is_none());
        assert!(data.children.is_empty());
        assert!(data.props.get("rows").is_none());
        assert!(data.props.get("source").is_none());
        assert!(data.props.get("transport").is_none());
        let components = data.props["viewBinding"]["surfaceComponents"]
            .as_array()
            .expect("compiler-owned component binding");
        assert_eq!(components.len(), 3);
        assert_eq!(components[0]["kind"], "section");
        assert_eq!(components[0]["label"], "Overview");
        assert_eq!(components[0]["children"][0]["kind"], "detail");
        assert_eq!(components[2]["columns"], json!(["status", "topic"]));

        let plan = surface.envelope().query_plan().unwrap();
        let request = plan
            .request(
                AppInstallationId::parse("install_surface_1").unwrap(),
                None,
                &BTreeMap::new(),
                None,
            )
            .unwrap();
        assert_eq!(request.limit, DEFAULT_SURFACE_PAGE_SIZE);

        let mut tampered = surface.envelope().clone();
        tampered.muij_document.layout[0].children[1].props["viewBinding"]["surfaceComponents"][0]
            ["label"] = json!("Substituted label");
        assert!(matches!(
            tampered.query_plan(),
            Err(AppSurfaceCompilerError::InvalidQueryPlan(_))
        ));
    }

    #[test]
    fn table_surface_uses_only_declared_columns_and_nested_route() {
        let document = valid_skill_document()
            .replace("kind: list", "kind: table")
            .replace(
                "      route: /",
                "      route: /records\n      columns: [topic]",
            );
        let surface = compile(&document, "plans", time(1));
        let data = data_component(&surface);
        assert_eq!(data.props["columns"].as_array().unwrap().len(), 1);
        assert_eq!(data.props["columns"][0]["key"], "topic");
        assert_eq!(data.props["columns"][0]["sortable"], true);
        let plan = surface.envelope().query_plan().unwrap();
        let request = plan
            .request(
                AppInstallationId::parse("install_surface_1").unwrap(),
                None,
                &BTreeMap::new(),
                None,
            )
            .unwrap();
        assert_eq!(
            request
                .select
                .iter()
                .map(AppFieldPath::as_str)
                .collect::<Vec<_>>(),
            vec!["status", "topic"]
        );
        let hidden_field = AppName::parse("status").unwrap();
        assert!(matches!(
            plan.request(
                AppInstallationId::parse("install_surface_1").unwrap(),
                None,
                &BTreeMap::new(),
                Some((&hidden_field, AppOrderDirection::Ascending)),
            ),
            Err(AppSurfaceQueryPlanError::InvalidReadIntent)
        ));
        assert_eq!(
            surface.binding().canonical_host_route,
            "/apps/install_surface_1/records"
        );
    }

    #[test]
    fn tree_surface_uses_canonical_text_label_and_structural_bindings() {
        let document = valid_skill_document()
            .replace(
                "        status: { type: enum, values: [new, done], required: true }",
                "        status: { type: enum, values: [new, done], required: true }\n        \
                 parent: { type: reference, entity: plan, nullable: true }\n        position: { \
                 type: integer, required: true }\n        notes: { type: markdown }\n        \
                 title: { type: text, required: true }",
            )
            .replace(
                "  workflows:\n",
                "    outline:\n      entity: plan\n      kind: tree\n      route: /outline\n      \
                 partition_field: topic\n      parent_field: parent\n      order_field: \
                 position\n      status_field: status\n  workflows:\n",
            );
        let surface = compile(&document, "outline", time(1));
        let data = data_component(&surface);
        assert_eq!(data.component_type, "Tree");
        assert_eq!(data.props["nodes"], json!([]));
        assert_eq!(data.props["expandAll"], false);
        assert_eq!(data.props["maxDepth"], DEFAULT_TREE_MAX_DEPTH);
        assert_eq!(data.props["viewBinding"]["labelField"], "title");
        assert_eq!(data.props["viewBinding"]["parentField"], "parent");
        assert_eq!(
            data.props["viewBinding"]["maxDepth"],
            DEFAULT_TREE_MAX_DEPTH
        );
        assert!(data.static_snapshot.is_none());
    }

    #[test]
    fn timeline_surface_maps_only_declared_activity_fields() {
        let document = valid_skill_document()
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
        let surface = compile(&document, "history", time(1));
        let data = data_component(&surface);
        assert_eq!(data.component_type, "ActivityFeed");
        assert_eq!(data.props["items"], json!([]));
        assert_eq!(data.props["maxItems"], DEFAULT_SURFACE_PAGE_SIZE);
        let binding = &data.props["viewBinding"];
        assert_eq!(binding["entity"], "plan");
        assert_eq!(binding["viewKind"], "timeline");
        assert_eq!(binding["fields"], json!(["occurred", "activity"]));
        assert_eq!(binding["timestampField"], "occurred");
        assert_eq!(binding["actionField"], "activity");
        assert_eq!(binding["defaultActor"], "Learning Plan");
        assert_eq!(binding["defaultType"], "update");
        assert!(binding["fieldBindings"]
            .as_array()
            .is_some_and(|bindings| bindings.len() == 4));
        assert!(!surface.envelope().query_plan().unwrap().mutation_allowed());
    }

    #[test]
    fn compiler_owned_view_binding_derives_the_canonical_query_request() {
        let surface = compile(&valid_skill_document(), "plans", time(1));
        let request = surface
            .envelope()
            .query_plan()
            .unwrap()
            .request(
                AppInstallationId::parse("install_surface_1").unwrap(),
                Some(AppReference::parse("cursor:surface:2").unwrap()),
                &BTreeMap::new(),
                None,
            )
            .unwrap();
        assert_eq!(request.entity.as_str(), "plan");
        assert_eq!(
            request
                .select
                .iter()
                .map(AppFieldPath::as_str)
                .collect::<Vec<_>>(),
            vec!["status", "topic"]
        );
        assert!(request.order.is_empty());
        assert_eq!(request.limit, DEFAULT_SURFACE_PAGE_SIZE);
        assert_eq!(request.cursor.unwrap().as_str(), "cursor:surface:2");
        assert_eq!(request.purpose.as_str(), APP_SURFACE_QUERY_PURPOSE);

        let sort_field = AppName::parse("topic").unwrap();
        let sorted = surface
            .envelope()
            .query_plan()
            .unwrap()
            .request(
                AppInstallationId::parse("install_surface_1").unwrap(),
                None,
                &BTreeMap::new(),
                Some((&sort_field, AppOrderDirection::Descending)),
            )
            .unwrap();
        assert_eq!(sorted.order[0].field.as_str(), "topic");
        assert_eq!(sorted.order[0].direction, AppOrderDirection::Descending);
        let undeclared = AppName::parse("undeclared").unwrap();
        assert!(matches!(
            surface.envelope().query_plan().unwrap().request(
                AppInstallationId::parse("install_surface_1").unwrap(),
                None,
                &BTreeMap::new(),
                Some((&undeclared, AppOrderDirection::Ascending)),
            ),
            Err(AppSurfaceQueryPlanError::InvalidReadIntent)
        ));
    }

    #[test]
    fn route_parameters_become_typed_bounded_query_predicates() {
        let document = valid_skill_document().replacen(
            "      route: /\n",
            "      route: /plans/:topic/:status\n",
            1,
        );
        let surface = compile(&document, "plans", time(1));
        assert_eq!(
            data_component(&surface).props["viewBinding"]["routeBindings"],
            json!([
                {
                    "parameter": "topic",
                    "field": "topic",
                    "scalarKind": "text",
                },
                {
                    "parameter": "status",
                    "field": "status",
                    "scalarKind": "enum",
                    "allowedValues": ["new", "done"],
                }
            ])
        );
        let parameters = BTreeMap::from([
            ("status".to_owned(), "new".to_owned()),
            ("topic".to_owned(), "personal-ai".to_owned()),
        ]);
        let request = surface
            .envelope()
            .query_plan()
            .unwrap()
            .request(
                AppInstallationId::parse("install_surface_1").unwrap(),
                None,
                &parameters,
                None,
            )
            .unwrap();
        let predicate = request.predicate.unwrap();
        assert_eq!(predicate.root, 0);
        assert!(matches!(
            &predicate.nodes[0],
            AppPredicateNode::All { children } if children == &[1, 2]
        ));
        assert!(matches!(
            &predicate.nodes[1],
            AppPredicateNode::Compare { field, operator: AppComparisonOperator::Equal, value }
                if field.as_str() == "topic" && value == "personal-ai"
        ));
        assert!(matches!(
            &predicate.nodes[2],
            AppPredicateNode::Compare { field, operator: AppComparisonOperator::Equal, value }
                if field.as_str() == "status" && value == "new"
        ));

        let invalid = BTreeMap::from([
            ("status".to_owned(), "unknown".to_owned()),
            ("topic".to_owned(), "personal-ai".to_owned()),
        ]);
        assert!(matches!(
            surface.envelope().query_plan().unwrap().request(
                AppInstallationId::parse("install_surface_1").unwrap(),
                None,
                &invalid,
                None,
            ),
            Err(AppSurfaceQueryPlanError::InvalidRouteParameters)
        ));
        assert!(matches!(
            surface
                .envelope()
                .query_plan()
                .unwrap()
                .validate_route_template("/plans/:status/:topic"),
            Err(AppSurfaceCompilerError::InvalidQueryPlan(_))
        ));
    }

    #[test]
    fn persisted_envelope_is_bounded_and_rejects_query_binding_drift() {
        let surface = compile(&valid_skill_document(), "plans", time(1));
        let encoded = serde_json::to_vec(surface.envelope()).unwrap();
        let decoded = AppSurfaceEnvelope::decode_persisted(&encoded).unwrap();
        assert_eq!(&decoded, surface.envelope());

        let mut value = serde_json::to_value(surface.envelope()).unwrap();
        value["muij_document"]["layout"][0]["children"][1]["props"]["viewBinding"]["undeclared"] =
            json!(true);
        let drifted =
            AppSurfaceEnvelope::decode_persisted(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(matches!(
            drifted.query_plan(),
            Err(AppSurfaceCompilerError::InvalidQueryPlan(_))
        ));
    }

    #[test]
    fn activation_retries_keep_exact_output_for_the_same_schema_evidence() {
        let first = compile(&valid_skill_document(), "plans", time(1));
        let retry = compile(&valid_skill_document(), "plans", time(1));
        assert_eq!(first, retry);
    }

    #[test]
    fn surface_set_compiles_every_route_once_under_one_stable_generation() {
        let document = valid_skill_document()
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
        let first = compile_set(&document, time(1));
        let retry = compile_set(&document, time(1));
        assert_eq!(
            first
                .surfaces()
                .keys()
                .map(AppName::as_str)
                .collect::<Vec<_>>(),
            vec!["history", "plans"]
        );
        assert_eq!(first.surface_revision(), AppRevision::new(3).unwrap());
        assert!(first
            .surfaces()
            .values()
            .all(|surface| surface.binding().surface_revision == first.surface_revision()));
        assert_eq!(
            first
                .surface(&AppName::parse("history").unwrap())
                .unwrap()
                .binding()
                .canonical_host_route,
            "/apps/install_surface_1/history"
        );
        assert_eq!(first.compiled_set_digest(), retry.compiled_set_digest());
    }

    #[test]
    fn published_surface_identity_is_stable_and_bounded_at_contract_limits() {
        let installation_id = AppInstallationId::parse(format!("i{}", "x".repeat(127))).unwrap();
        let view_id = AppName::parse(format!("v{}", "y".repeat(63))).unwrap();

        let first = app_published_surface_ref(&installation_id, &view_id).unwrap();
        let retry = app_published_surface_ref(&installation_id, &view_id).unwrap();
        let different_view = AppName::parse(format!("v{}z", "y".repeat(62))).unwrap();
        let different = app_published_surface_ref(&installation_id, &different_view).unwrap();

        assert_eq!(first, retry);
        assert_ne!(first, different);
        assert!(first.as_str().len() <= 192);
    }

    #[test]
    fn component_identity_is_namespaced_by_installation_view_and_schema() {
        let installation = AppInstallationId::parse("install_a").unwrap();
        let view = AppName::parse("plans").unwrap();
        let base = component_id(&installation, &view, AppRevision::new(1).unwrap(), "data");
        assert_ne!(
            base,
            component_id(
                &AppInstallationId::parse("install_b").unwrap(),
                &view,
                AppRevision::new(1).unwrap(),
                "data"
            )
        );
        assert_ne!(
            base,
            component_id(
                &installation,
                &AppName::parse("history").unwrap(),
                AppRevision::new(1).unwrap(),
                "data"
            )
        );
        assert_ne!(
            base,
            component_id(&installation, &view, AppRevision::new(2).unwrap(), "data")
        );
    }

    #[test]
    fn list_surface_rejects_schema_width_before_allocating_renderer_rows() {
        let fields = (0..=MAX_SURFACE_FIELDS)
            .map(|index| format!("        field_{index}: {{ type: text }}"))
            .collect::<Vec<_>>()
            .join("\n");
        let document = valid_skill_document().replace(
            "        topic: { type: text, required: true }\n        status: { type: enum, values: \
             [new, done], required: true }",
            &fields,
        );
        assert!(matches!(
            compile_result(&document, "plans", time(1)),
            Err(AppSurfaceCompilerError::SurfaceFieldLimit {
                actual,
                limit: MAX_SURFACE_FIELDS,
                ..
            }) if actual == MAX_SURFACE_FIELDS + 1
        ));
    }

    #[test]
    fn envelope_serializes_only_the_versioned_app_adapter_contract() {
        static_assertions::assert_not_impl_any!(
            VerifiedAppSurfaceSource: Clone, serde::de::DeserializeOwned
        );
        let surface = compile(&valid_skill_document(), "plans", time(1));
        let encoded = serde_json::to_value(surface.envelope()).unwrap();
        let object = encoded.as_object().unwrap();
        assert_eq!(object.len(), 5);
        for key in [
            "installation_id",
            "interaction_mode",
            "muij_document",
            "surface_revision",
            "view_id",
        ] {
            assert!(object.contains_key(key), "missing envelope field {key}");
        }
        assert_eq!(encoded["interaction_mode"], "app");
        assert!(encoded.get("scope").is_none());
        assert!(encoded.get("grant_revision").is_none());
    }

    #[test]
    fn missing_view_fails_without_falling_back_to_the_first_manifest_view() {
        let manifest = parse_app_manifest_frontmatter(
            valid_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .unwrap();
        let result = compile_app_surface_preview(
            &manifest,
            AppInstallationId::parse("install_surface_1").unwrap(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
            &AppName::parse("missing").unwrap(),
            time(1),
        );
        assert!(
            matches!(result, Err(AppSurfaceCompilerError::ViewNotFound(view)) if view == "missing")
        );
    }

    #[test]
    fn immutable_package_schema_mismatch_fails_before_view_compilation() {
        let manifest = parse_app_manifest_frontmatter(
            valid_skill_document().as_bytes(),
            &AppPackageLimits::default(),
        )
        .unwrap();
        let installation = AppInstallationId::parse("install_surface_1").unwrap();
        let package = AppReference::parse("package:surface:1").unwrap();
        let schema = AppRevision::new(1).unwrap();
        let entity_digest = canonical_entity_schema_digest(manifest.manifest()).unwrap();
        let view_digest = canonical_view_schema_digest(manifest.manifest()).unwrap();

        let source = VerifiedAppSurfaceSource::for_test(
            manifest.clone(),
            installation.clone(),
            package.clone(),
            schema,
            time(1),
            AppDigest::blake3(b"wrong-entity-schema"),
            view_digest.clone(),
        );
        assert!(matches!(
            compile_app_surface_set(&source, AppRevision::new(1).unwrap()),
            Err(AppSurfaceCompilerError::EntitySchemaDigestMismatch)
        ));
        let source = VerifiedAppSurfaceSource::for_test(
            manifest,
            installation,
            package,
            schema,
            time(1),
            entity_digest,
            AppDigest::blake3(b"wrong-view-schema"),
        );
        assert!(matches!(
            compile_app_surface_set(&source, AppRevision::new(1).unwrap()),
            Err(AppSurfaceCompilerError::ViewSchemaDigestMismatch)
        ));
    }

    fn graph_skill_document() -> String {
        valid_skill_document()
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
            )
    }

    #[test]
    fn graph_surface_compiles_into_the_muij_graph_family() {
        let surface = compile(&graph_skill_document(), "outline", time(1));
        let data = data_component(&surface);
        assert_eq!(data.component_type, "Graph");
        assert_eq!(data.source.as_deref(), Some(APP_QUERY_SOURCE));
        assert_eq!(data.query.as_deref(), Some("view:outline"));
        // The skeleton props decode into the exact MUIJ graph spec shape the
        // native renderers consume; unknown keys (the view binding) are
        // tolerated exactly as every other data component tolerates them.
        let spec = crate::magician_v2::gaui::muij::MuijGraphSpec::from_props(&data.props)
            .expect("graph props decode into the MUIJ graph spec");
        assert!(spec.nodes.is_empty());
        assert!(spec.edges.is_empty());
        assert_eq!(
            spec.layout,
            crate::magician_v2::gaui::muij::MuijGraphLayout::Layered
        );
        assert!(spec.focus_node_id.is_none());
        assert!(spec.reveal_order.is_empty());

        let plan = surface.envelope().query_plan().expect("graph query plan");
        assert!(plan.requires_complete_page());
        assert!(!plan.mutation_allowed());
        assert_eq!(plan.order.len(), 1);
        assert_eq!(plan.order[0].field.as_str(), "position");
        // The projection is exactly the label plus the structural bindings.
        assert_eq!(
            plan.visible_fields
                .iter()
                .map(|field| field.as_str())
                .collect::<Vec<_>>(),
            vec!["topic", "parent", "position", "status"]
        );
        // The view binding mirrors the tree's label discipline: label,
        // parent edge source, deterministic order and an optional enum kind.
        let binding = data.props.get("viewBinding").unwrap();
        assert_eq!(binding["labelField"], json!("topic"));
        assert_eq!(binding["parentField"], json!("parent"));
        assert_eq!(binding["orderField"], json!("position"));
        assert_eq!(binding["statusField"], json!("status"));
        assert_eq!(binding["viewKind"], json!("graph"));
    }

    #[test]
    fn graph_surfaces_stay_read_only_and_assemble_from_complete_pages() {
        // The graph family's bounded interactions (select/expand/focus/
        // reveal) carry no mutations, and a graph cannot assemble its
        // parent→child edges from a paged subset — the same complete-page
        // contract trees already carry.
        let surface = compile(&graph_skill_document(), "outline", time(2));
        let plan = surface.envelope().query_plan().expect("graph query plan");
        assert!(!plan.mutation_allowed());
        assert!(plan.requires_complete_page());
        let select = plan
            .select
            .iter()
            .map(|field| field.as_str())
            .collect::<Vec<_>>();
        assert_eq!(select, vec!["topic", "parent", "position", "status"]);
    }
}
