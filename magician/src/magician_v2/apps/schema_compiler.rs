//! Deterministic Phase-2 app entity-schema compiler.
//!
//! The compiler consumes only a manifest that already passed the strict
//! package parser plus server-selected installation/revision/policy identity.
//! It emits the persisted [`AppSchemaRevision`] and non-transport typed-query
//! contracts from one normalized field graph. It performs no filesystem,
//! SQLite, provider, model or vector-index work.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, OnceLock},
};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    manifest::{
        AppManifestField, AppManifestPolicyOverride, AppPackageManifest, AppReferenceCyclePolicy,
        AppReferenceDeletePolicy,
    },
    models::{
        AppContractError, AppContractLimits, AppDigest, AppFieldPath, AppInstallationId, AppName,
        AppReference, AppRevision, ValidateAppContract,
    },
    query_semantics::{
        AppQueryFieldContract, AppQueryRelationContract, AppQueryScalarKind,
        AppQuerySemanticsError, AppTypedQuerySchema,
    },
    records::{
        AppDataHandlingPolicy, AppExternalEgress, AppSchemaCompatibility, AppSchemaRevision,
    },
};

const COMPILED_SCHEMA_VERSION: u32 = 2;
const RUNTIME_CONTRACT_CACHE_ENTRIES: usize = 128;
const RUNTIME_CONTRACT_CACHE_BYTES: usize = 8 * 1_024 * 1_024;

struct RuntimeContractCacheEntry {
    contracts: Arc<BTreeMap<AppName, AppEntityRuntimeContract>>,
    source_bytes: usize,
    last_used: u64,
}

#[derive(Default)]
struct RuntimeContractCache {
    entries: BTreeMap<AppDigest, RuntimeContractCacheEntry>,
    source_bytes: usize,
    clock: u64,
}

static RUNTIME_CONTRACT_CACHE: OnceLock<Mutex<RuntimeContractCache>> = OnceLock::new();

/// Server-owned result of compiling one exact package schema revision.
///
/// The query contracts are deliberately not deserializable. A future read
/// adapter must rebuild them from the validated persisted schema at the
/// current authority boundary instead of trusting caller-provided plans.
#[derive(Debug, Clone)]
pub struct CompiledAppSchema {
    revision: AppSchemaRevision,
    entity_schema_digest: AppDigest,
    #[cfg_attr(not(test), allow(dead_code))]
    query_schemas: BTreeMap<AppName, AppTypedQuerySchema>,
}

impl CompiledAppSchema {
    pub fn revision(&self) -> &AppSchemaRevision {
        &self.revision
    }

    pub fn entity_schema_digest(&self) -> &AppDigest {
        &self.entity_schema_digest
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn query_schema(&self, entity: &AppName) -> Option<&AppTypedQuerySchema> {
        self.query_schemas.get(entity)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn into_revision(self) -> AppSchemaRevision {
        self.revision
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompiledEntitySchemaDocument {
    schema_version: u32,
    source_entity_schema_digest: AppDigest,
    entities: BTreeMap<AppName, CompiledEntitySchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompiledEntitySchema {
    fields: BTreeMap<AppName, CompiledFieldSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompiledFieldSchema {
    kind: AppQueryScalarKind,
    required: bool,
    nullable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    enum_values: Vec<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reference_entity: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reference_delete_policy: Option<AppReferenceDeletePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reference_cycle_policy: Option<AppReferenceCyclePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relation_max_depth: Option<u16>,
    effective_policy: AppDataHandlingPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompiledIndexPlanDocument {
    schema_version: u32,
    entities: BTreeMap<AppName, CompiledEntityIndexPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompiledEntityIndexPlan {
    scalar_fields: Vec<AppFieldPath>,
    sortable_fields: Vec<AppFieldPath>,
    text_search_fields: Vec<AppFieldPath>,
    relation_fields: Vec<AppFieldPath>,
}

struct FieldParts<'a> {
    kind: AppQueryScalarKind,
    required: bool,
    nullable: bool,
    enum_values: BTreeSet<AppName>,
    reference: Option<(
        &'a AppName,
        AppReferenceDeletePolicy,
        AppReferenceCyclePolicy,
        u16,
    )>,
    policy: Option<&'a AppManifestPolicyOverride>,
}

struct CompiledSchemaParts {
    canonical_entity_schema: Value,
    compiled_index_plan: Value,
    query_schemas: BTreeMap<AppName, AppTypedQuerySchema>,
}

/// Compile one exact schema revision and verify it matches the immutable
/// entity-schema digest recorded by the package revision.
#[allow(clippy::too_many_arguments)]
pub fn compile_app_schema(
    manifest: &AppPackageManifest,
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    effective_policy: AppDataHandlingPolicy,
    compatibility: AppSchemaCompatibility,
    migration_plan_ref: Option<AppReference>,
    expected_entity_schema_digest: &AppDigest,
    created_at: DateTime<Utc>,
) -> Result<CompiledAppSchema, AppSchemaCompilerError> {
    let entity_schema_digest = canonical_entity_schema_digest(manifest)?;
    if &entity_schema_digest != expected_entity_schema_digest {
        return Err(AppSchemaCompilerError::EntitySchemaDigestMismatch);
    }
    let parts = build_schema_parts(manifest, &effective_policy, entity_schema_digest.clone())?;
    let revision = AppSchemaRevision {
        installation_id,
        revision: schema_revision,
        package_revision_ref,
        canonical_entity_schema: parts.canonical_entity_schema.clone(),
        canonical_data_handling_policy: effective_policy,
        compiled_validation_schema: parts.canonical_entity_schema,
        compiled_index_plan: parts.compiled_index_plan,
        compatibility_with_previous: compatibility,
        migration_plan_ref,
        created_at,
    };
    revision.validate_app_contract(&AppContractLimits::default())?;

    Ok(CompiledAppSchema {
        revision,
        entity_schema_digest,
        query_schemas: parts.query_schemas,
    })
}

/// Compute the exact digest a package revision persists for its admitted
/// entity declarations. Installation grants and wall-clock time cannot alter
/// immutable package identity.
pub fn canonical_entity_schema_digest(
    manifest: &AppPackageManifest,
) -> Result<AppDigest, AppSchemaCompilerError> {
    let canonical = serde_json::to_value(&manifest.app.entities)?;
    Ok(AppDigest::blake3_canonical_json(&canonical)?)
}

fn build_schema_parts(
    manifest: &AppPackageManifest,
    effective_policy: &AppDataHandlingPolicy,
    source_entity_schema_digest: AppDigest,
) -> Result<CompiledSchemaParts, AppSchemaCompilerError> {
    let mut entities = BTreeMap::new();
    let mut index_entities = BTreeMap::new();

    for (entity_name, entity) in &manifest.app.entities {
        let entity_policy = restrict_policy(effective_policy.clone(), entity.data_policy.as_ref());
        let indexed_field_names = indexed_fields_for_entity(manifest, entity_name);
        let mut fields = BTreeMap::new();
        let mut scalar_fields = Vec::with_capacity(entity.fields.len());
        let mut sortable_fields = Vec::with_capacity(entity.fields.len());
        let mut text_search_fields = Vec::new();
        let mut relation_fields = Vec::new();

        for (field_name, field) in &entity.fields {
            let parts = field_parts(field);
            let field_path = AppFieldPath::parse(field_name.as_str())?;
            let field_policy = restrict_policy(entity_policy.clone(), parts.policy);
            let indexed = indexed_field_names.contains(field_name)
                || parts.kind == AppQueryScalarKind::Reference;
            let sortable = indexed && parts.kind != AppQueryScalarKind::Markdown;
            if indexed {
                scalar_fields.push(field_path.clone());
            }
            if sortable {
                sortable_fields.push(field_path.clone());
            }
            if indexed
                && matches!(
                    parts.kind,
                    AppQueryScalarKind::Text | AppQueryScalarKind::Markdown
                )
            {
                text_search_fields.push(field_path.clone());
            }

            let (
                reference_entity,
                reference_delete_policy,
                reference_cycle_policy,
                relation_max_depth,
            ) = if let Some((target, delete_policy, cycle_policy, max_depth)) = parts.reference {
                if !manifest.app.entities.contains_key(target) {
                    return Err(AppSchemaCompilerError::MissingReferenceTarget(
                        target.to_string(),
                    ));
                }
                relation_fields.push(field_path);
                (
                    Some(target.clone()),
                    Some(delete_policy),
                    Some(cycle_policy),
                    Some(max_depth),
                )
            } else {
                (None, None, None, None)
            };

            fields.insert(
                field_name.clone(),
                CompiledFieldSchema {
                    kind: parts.kind,
                    required: parts.required,
                    nullable: parts.nullable,
                    enum_values: parts.enum_values.into_iter().collect(),
                    reference_entity,
                    reference_delete_policy,
                    reference_cycle_policy,
                    relation_max_depth,
                    effective_policy: field_policy,
                },
            );
        }

        entities.insert(entity_name.clone(), CompiledEntitySchema { fields });
        index_entities.insert(
            entity_name.clone(),
            CompiledEntityIndexPlan {
                scalar_fields,
                sortable_fields,
                text_search_fields,
                relation_fields,
            },
        );
    }

    let entity_document = CompiledEntitySchemaDocument {
        schema_version: COMPILED_SCHEMA_VERSION,
        source_entity_schema_digest,
        entities,
    };
    let index_document = CompiledIndexPlanDocument {
        schema_version: COMPILED_SCHEMA_VERSION,
        entities: index_entities,
    };
    let query_schemas = query_schemas_from_documents(&entity_document, &index_document)?;
    let canonical_entity_schema = serde_json::to_value(entity_document)?;
    let compiled_index_plan = serde_json::to_value(index_document)?;
    Ok(CompiledSchemaParts {
        canonical_entity_schema,
        compiled_index_plan,
        query_schemas,
    })
}

fn indexed_fields_for_entity(
    manifest: &AppPackageManifest,
    entity_name: &AppName,
) -> BTreeSet<AppName> {
    let mut indexed = BTreeSet::new();
    for view in manifest
        .app
        .views
        .values()
        .filter(|view| &view.entity == entity_name)
    {
        indexed.extend(view.columns.iter().cloned());
        indexed.extend(
            [
                view.partition_field.as_ref(),
                view.parent_field.as_ref(),
                view.order_field.as_ref(),
                view.status_field.as_ref(),
                view.timestamp_field.as_ref(),
                view.action_field.as_ref(),
                view.actor_field.as_ref(),
                view.type_field.as_ref(),
                view.target_field.as_ref(),
            ]
            .into_iter()
            .flatten()
            .cloned(),
        );
    }
    indexed
}

/// Rebuild non-transport query contracts from one validated persisted schema.
/// No caller-supplied executable query plan crosses this boundary.
pub fn query_schemas_from_revision(
    revision: &AppSchemaRevision,
) -> Result<BTreeMap<AppName, AppTypedQuerySchema>, AppSchemaCompilerError> {
    revision.validate_app_contract(&AppContractLimits::default())?;
    if revision.canonical_entity_schema != revision.compiled_validation_schema {
        return Err(AppSchemaCompilerError::InvalidCompiledSchema(
            "validation schema differs from canonical entity schema",
        ));
    }
    let entities: CompiledEntitySchemaDocument =
        serde_json::from_value(revision.canonical_entity_schema.clone())?;
    let indexes: CompiledIndexPlanDocument =
        serde_json::from_value(revision.compiled_index_plan.clone())?;
    query_schemas_from_documents(&entities, &indexes)
}

pub fn source_entity_schema_digest_from_revision(
    revision: &AppSchemaRevision,
) -> Result<AppDigest, AppSchemaCompilerError> {
    revision.validate_app_contract(&AppContractLimits::default())?;
    let entities: CompiledEntitySchemaDocument =
        serde_json::from_value(revision.canonical_entity_schema.clone())?;
    if entities.schema_version != COMPILED_SCHEMA_VERSION {
        return Err(AppSchemaCompilerError::InvalidCompiledSchema(
            "unsupported compiled schema version",
        ));
    }
    Ok(entities.source_entity_schema_digest)
}

#[derive(Debug, Clone)]
pub struct AppEntityRuntimeContract {
    query_schema: AppTypedQuerySchema,
    fields: BTreeMap<AppFieldPath, AppRuntimeFieldContract>,
}

impl AppEntityRuntimeContract {
    pub fn query_schema(&self) -> &AppTypedQuerySchema {
        &self.query_schema
    }

    pub fn field(&self, path: &AppFieldPath) -> Option<&AppRuntimeFieldContract> {
        self.fields.get(path)
    }

    pub fn fields(&self) -> impl Iterator<Item = (&AppFieldPath, &AppRuntimeFieldContract)> {
        self.fields.iter()
    }

    /// Revalidate persisted data against the exact active compiled schema.
    ///
    /// This is intentionally iterative and top-level: V1 entity declarations
    /// contain scalar fields only. Reads and future writes share this method so
    /// corrupt storage can never bypass the same required/null/type/enum and
    /// reference checks applied at admission.
    pub fn validate_payload(&self, payload: &Value) -> Result<(), AppSchemaCompilerError> {
        let object = payload.as_object().ok_or_else(|| {
            AppSchemaCompilerError::InvalidRecordPayload("record payload is not an object".into())
        })?;
        if let Some(unknown) = object.keys().find(|name| {
            !self
                .fields
                .keys()
                .any(|field| field.as_str() == name.as_str())
        }) {
            return Err(AppSchemaCompilerError::InvalidRecordPayload(format!(
                "record payload contains unknown field `{unknown}`"
            )));
        }
        for (path, field) in &self.fields {
            let Some(value) = object.get(path.as_str()) else {
                if field.required {
                    return Err(AppSchemaCompilerError::InvalidRecordPayload(format!(
                        "record payload is missing required field `{path}`"
                    )));
                }
                continue;
            };
            if value.is_null() {
                if !field.nullable {
                    return Err(AppSchemaCompilerError::InvalidRecordPayload(format!(
                        "record payload field `{path}` is not nullable"
                    )));
                }
                continue;
            }
            if !field.accepts_value(value) {
                return Err(AppSchemaCompilerError::InvalidRecordPayload(format!(
                    "record payload field `{path}` does not match its compiled type"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AppRuntimeFieldContract {
    kind: AppQueryScalarKind,
    required: bool,
    nullable: bool,
    enum_values: BTreeSet<AppName>,
    indexed: bool,
    text_search: bool,
    effective_policy: AppDataHandlingPolicy,
    reference_entity: Option<AppName>,
    reference_delete_policy: Option<AppReferenceDeletePolicy>,
    reference_cycle_policy: Option<AppReferenceCyclePolicy>,
    relation_max_depth: Option<u16>,
}

impl AppRuntimeFieldContract {
    fn accepts_value(&self, value: &Value) -> bool {
        match self.kind {
            AppQueryScalarKind::Text | AppQueryScalarKind::Markdown => value.is_string(),
            AppQueryScalarKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
            AppQueryScalarKind::Decimal => {
                value.is_number() && value.to_string().parse::<Decimal>().is_ok()
            },
            AppQueryScalarKind::Boolean => value.is_boolean(),
            AppQueryScalarKind::Timestamp => value
                .as_str()
                .is_some_and(|value| DateTime::parse_from_rfc3339(value).is_ok()),
            AppQueryScalarKind::Enum => value.as_str().is_some_and(|value| {
                AppName::parse(value)
                    .ok()
                    .is_some_and(|value| self.enum_values.contains(&value))
            }),
            AppQueryScalarKind::Reference => value
                .as_str()
                .is_some_and(|value| super::models::AppRecordId::parse(value).is_ok()),
        }
    }

    pub fn kind(&self) -> AppQueryScalarKind {
        self.kind
    }

    pub fn required(&self) -> bool {
        self.required
    }

    pub fn nullable(&self) -> bool {
        self.nullable
    }

    pub fn enum_values(&self) -> &BTreeSet<AppName> {
        &self.enum_values
    }

    pub fn indexed(&self) -> bool {
        self.indexed
    }

    pub fn text_search(&self) -> bool {
        self.text_search
    }

    pub fn effective_policy(&self) -> &AppDataHandlingPolicy {
        &self.effective_policy
    }

    pub fn reference_entity(&self) -> Option<&AppName> {
        self.reference_entity.as_ref()
    }

    pub fn reference_delete_policy(&self) -> Option<AppReferenceDeletePolicy> {
        self.reference_delete_policy
    }

    pub fn reference_cycle_policy(&self) -> Option<AppReferenceCyclePolicy> {
        self.reference_cycle_policy
    }

    pub fn relation_max_depth(&self) -> Option<u16> {
        self.relation_max_depth
    }
}

pub fn runtime_contracts_from_revision(
    revision: &AppSchemaRevision,
) -> Result<Arc<BTreeMap<AppName, AppEntityRuntimeContract>>, AppSchemaCompilerError> {
    revision.validate_app_contract(&AppContractLimits::default())?;
    // This equality is a persisted integrity invariant, not merely part of
    // rebuilding a cache miss. Check it before the cache lookup so a warm
    // entry cannot make a tampered validation document look admissible.
    if revision.canonical_entity_schema != revision.compiled_validation_schema {
        return Err(AppSchemaCompilerError::InvalidCompiledSchema(
            "validation schema differs from canonical entity schema",
        ));
    }
    let cache_material = serde_json::json!({
        "entity_schema": &revision.canonical_entity_schema,
        "index_plan": &revision.compiled_index_plan,
    });
    let cache_key = AppDigest::blake3_canonical_json(&cache_material)?;
    let source_bytes = serde_json::to_vec(&cache_material)?.len();
    let cache = RUNTIME_CONTRACT_CACHE.get_or_init(|| Mutex::new(RuntimeContractCache::default()));
    {
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clock = cache.clock.saturating_add(1);
        let now = cache.clock;
        if let Some(entry) = cache.entries.get_mut(&cache_key) {
            entry.last_used = now;
            return Ok(Arc::clone(&entry.contracts));
        }
    }
    let query_schemas = query_schemas_from_revision(revision)?;
    let entities: CompiledEntitySchemaDocument =
        serde_json::from_value(revision.canonical_entity_schema.clone())?;
    let indexes: CompiledIndexPlanDocument =
        serde_json::from_value(revision.compiled_index_plan.clone())?;
    let mut runtime = BTreeMap::new();
    for (entity_name, entity) in entities.entities {
        let query_schema = query_schemas.get(&entity_name).cloned().ok_or(
            AppSchemaCompilerError::InvalidCompiledSchema("entity has no typed query schema"),
        )?;
        let index = indexes.entities.get(&entity_name).ok_or(
            AppSchemaCompilerError::InvalidCompiledSchema("entity has no runtime index plan"),
        )?;
        let scalar_fields = index.scalar_fields.iter().cloned().collect::<BTreeSet<_>>();
        let text_search_fields = index
            .text_search_fields
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let fields = entity
            .fields
            .into_iter()
            .map(|(name, field)| {
                let path = AppFieldPath::parse(name.as_str())?;
                Ok((
                    path.clone(),
                    AppRuntimeFieldContract {
                        kind: field.kind,
                        required: field.required,
                        nullable: field.nullable,
                        enum_values: field.enum_values.into_iter().collect(),
                        indexed: scalar_fields.contains(&path),
                        text_search: text_search_fields.contains(&path),
                        effective_policy: field.effective_policy,
                        reference_entity: field.reference_entity,
                        reference_delete_policy: field.reference_delete_policy,
                        reference_cycle_policy: field.reference_cycle_policy,
                        relation_max_depth: field.relation_max_depth,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, AppSchemaCompilerError>>()?;
        runtime.insert(
            entity_name,
            AppEntityRuntimeContract {
                query_schema,
                fields,
            },
        );
    }
    let runtime = Arc::new(runtime);
    if source_bytes <= RUNTIME_CONTRACT_CACHE_BYTES {
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clock = cache.clock.saturating_add(1);
        let now = cache.clock;
        // Another request may have compiled and inserted this exact schema
        // while this request was outside the lock. Reuse it without charging
        // the same bytes twice or perturbing eviction accounting.
        if let Some(entry) = cache.entries.get_mut(&cache_key) {
            entry.last_used = now;
            return Ok(Arc::clone(&entry.contracts));
        }
        while !cache.entries.is_empty()
            && (cache.entries.len() >= RUNTIME_CONTRACT_CACHE_ENTRIES
                || cache.source_bytes.saturating_add(source_bytes) > RUNTIME_CONTRACT_CACHE_BYTES)
        {
            let oldest = cache
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else {
                break;
            };
            if let Some(removed) = cache.entries.remove(&oldest) {
                cache.source_bytes = cache.source_bytes.saturating_sub(removed.source_bytes);
            }
        }
        cache.source_bytes = cache.source_bytes.saturating_add(source_bytes);
        cache.entries.insert(
            cache_key,
            RuntimeContractCacheEntry {
                contracts: Arc::clone(&runtime),
                source_bytes,
                last_used: now,
            },
        );
    }
    Ok(runtime)
}

fn query_schemas_from_documents(
    entity_document: &CompiledEntitySchemaDocument,
    index_document: &CompiledIndexPlanDocument,
) -> Result<BTreeMap<AppName, AppTypedQuerySchema>, AppSchemaCompilerError> {
    if entity_document.schema_version != COMPILED_SCHEMA_VERSION
        || index_document.schema_version != COMPILED_SCHEMA_VERSION
        || entity_document
            .entities
            .keys()
            .ne(index_document.entities.keys())
    {
        return Err(AppSchemaCompilerError::InvalidCompiledSchema(
            "entity and index schema identities differ",
        ));
    }

    let mut schemas = BTreeMap::new();
    for (entity_name, entity) in &entity_document.entities {
        let index = index_document.entities.get(entity_name).ok_or(
            AppSchemaCompilerError::InvalidCompiledSchema("entity has no index plan"),
        )?;
        let scalar_fields = unique_paths(&index.scalar_fields)?;
        let sortable_fields = unique_paths(&index.sortable_fields)?;
        let text_search_fields = unique_paths(&index.text_search_fields)?;
        let relation_fields = unique_paths(&index.relation_fields)?;
        if !sortable_fields.is_subset(&scalar_fields)
            || !text_search_fields.is_subset(&scalar_fields)
        {
            return Err(AppSchemaCompilerError::InvalidCompiledSchema(
                "specialized indexes are not scalar-index subsets",
            ));
        }

        let mut field_contracts = Vec::with_capacity(entity.fields.len());
        let mut relation_contracts = Vec::new();
        let mut expected_relation_fields = BTreeSet::new();
        for (field_name, field) in &entity.fields {
            let path = AppFieldPath::parse(field_name.as_str())?;
            let indexed = scalar_fields.contains(&path);
            let sortable = sortable_fields.contains(&path);
            if sortable && field.kind == AppQueryScalarKind::Markdown {
                return Err(AppSchemaCompilerError::InvalidCompiledSchema(
                    "markdown field cannot be sortable",
                ));
            }
            if text_search_fields.contains(&path)
                && !matches!(
                    field.kind,
                    AppQueryScalarKind::Text | AppQueryScalarKind::Markdown
                )
            {
                return Err(AppSchemaCompilerError::InvalidCompiledSchema(
                    "text-search index targets a non-text field",
                ));
            }
            let contract = if field.kind == AppQueryScalarKind::Enum {
                AppQueryFieldContract::from_compiled_enum_schema(
                    path.clone(),
                    field.nullable,
                    indexed,
                    sortable,
                    field.enum_values.iter().cloned().collect(),
                )?
            } else {
                if !field.enum_values.is_empty() {
                    return Err(AppSchemaCompilerError::InvalidCompiledSchema(
                        "non-enum field carries enum values",
                    ));
                }
                AppQueryFieldContract::from_compiled_schema(
                    path.clone(),
                    field.kind,
                    field.nullable,
                    indexed,
                    sortable,
                )
            };
            field_contracts.push(contract);

            if field.kind == AppQueryScalarKind::Reference {
                expected_relation_fields.insert(path);
                let target = field.reference_entity.as_ref().ok_or(
                    AppSchemaCompilerError::InvalidCompiledSchema(
                        "reference field has no target entity",
                    ),
                )?;
                let target_entity = entity_document.entities.get(target).ok_or_else(|| {
                    AppSchemaCompilerError::MissingReferenceTarget(target.to_string())
                })?;
                let selectable_fields = target_entity
                    .fields
                    .keys()
                    .map(|name| AppFieldPath::parse(name.as_str()))
                    .collect::<Result<BTreeSet<_>, _>>()?;
                let max_depth = field.relation_max_depth.ok_or(
                    AppSchemaCompilerError::InvalidCompiledSchema(
                        "reference field has no traversal bound",
                    ),
                )?;
                if field.reference_cycle_policy.is_none() {
                    return Err(AppSchemaCompilerError::InvalidCompiledSchema(
                        "reference field has no cycle policy",
                    ));
                }
                if field.reference_delete_policy.is_none() {
                    return Err(AppSchemaCompilerError::InvalidCompiledSchema(
                        "reference field has no delete policy",
                    ));
                }
                relation_contracts.push(AppQueryRelationContract::from_compiled_schema(
                    field_name.clone(),
                    selectable_fields,
                    max_depth,
                )?);
            } else if field.reference_entity.is_some()
                || field.reference_delete_policy.is_some()
                || field.reference_cycle_policy.is_some()
                || field.relation_max_depth.is_some()
            {
                return Err(AppSchemaCompilerError::InvalidCompiledSchema(
                    "non-reference field carries relation metadata",
                ));
            }
        }

        let known_fields = entity
            .fields
            .keys()
            .map(|name| AppFieldPath::parse(name.as_str()))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if !scalar_fields.is_subset(&known_fields)
            || !text_search_fields.is_subset(&known_fields)
            || relation_fields != expected_relation_fields
        {
            return Err(AppSchemaCompilerError::InvalidCompiledSchema(
                "index plan references unknown or inconsistent fields",
            ));
        }
        schemas.insert(
            entity_name.clone(),
            AppTypedQuerySchema::from_compiled_contracts(field_contracts, relation_contracts)?,
        );
    }
    Ok(schemas)
}

fn unique_paths(paths: &[AppFieldPath]) -> Result<BTreeSet<AppFieldPath>, AppSchemaCompilerError> {
    let unique = paths.iter().cloned().collect::<BTreeSet<_>>();
    if unique.len() != paths.len() {
        return Err(AppSchemaCompilerError::InvalidCompiledSchema(
            "index plan contains duplicate field paths",
        ));
    }
    Ok(unique)
}

fn field_parts(field: &AppManifestField) -> FieldParts<'_> {
    match field {
        AppManifestField::Text {
            required,
            nullable,
            data_policy,
        } => FieldParts {
            kind: AppQueryScalarKind::Text,
            required: *required,
            nullable: *nullable,
            enum_values: BTreeSet::new(),
            reference: None,
            policy: data_policy.as_ref(),
        },
        AppManifestField::Markdown {
            required,
            nullable,
            data_policy,
        } => FieldParts {
            kind: AppQueryScalarKind::Markdown,
            required: *required,
            nullable: *nullable,
            enum_values: BTreeSet::new(),
            reference: None,
            policy: data_policy.as_ref(),
        },
        AppManifestField::Integer {
            required,
            nullable,
            data_policy,
        } => FieldParts {
            kind: AppQueryScalarKind::Integer,
            required: *required,
            nullable: *nullable,
            enum_values: BTreeSet::new(),
            reference: None,
            policy: data_policy.as_ref(),
        },
        AppManifestField::Decimal {
            required,
            nullable,
            data_policy,
        } => FieldParts {
            kind: AppQueryScalarKind::Decimal,
            required: *required,
            nullable: *nullable,
            enum_values: BTreeSet::new(),
            reference: None,
            policy: data_policy.as_ref(),
        },
        AppManifestField::Boolean {
            required,
            nullable,
            data_policy,
        } => FieldParts {
            kind: AppQueryScalarKind::Boolean,
            required: *required,
            nullable: *nullable,
            enum_values: BTreeSet::new(),
            reference: None,
            policy: data_policy.as_ref(),
        },
        AppManifestField::Timestamp {
            required,
            nullable,
            data_policy,
        } => FieldParts {
            kind: AppQueryScalarKind::Timestamp,
            required: *required,
            nullable: *nullable,
            enum_values: BTreeSet::new(),
            reference: None,
            policy: data_policy.as_ref(),
        },
        AppManifestField::Enum {
            values,
            required,
            nullable,
            data_policy,
        } => FieldParts {
            kind: AppQueryScalarKind::Enum,
            required: *required,
            nullable: *nullable,
            enum_values: values.iter().cloned().collect(),
            reference: None,
            policy: data_policy.as_ref(),
        },
        AppManifestField::Reference {
            entity,
            required,
            nullable,
            on_delete,
            cycle_policy,
            max_traversal_depth,
            data_policy,
            ..
        } => FieldParts {
            kind: AppQueryScalarKind::Reference,
            required: *required,
            nullable: *nullable,
            enum_values: BTreeSet::new(),
            reference: Some((entity, *on_delete, *cycle_policy, *max_traversal_depth)),
            policy: data_policy.as_ref(),
        },
    }
}

pub(crate) fn restrict_policy(
    mut base: AppDataHandlingPolicy,
    restriction: Option<&AppManifestPolicyOverride>,
) -> AppDataHandlingPolicy {
    let Some(restriction) = restriction else {
        base.approved_destinations.sort();
        base.approved_destinations.dedup();
        return base;
    };
    if let Some(value) = restriction.classification_floor {
        base.classification_floor = base.classification_floor.max(value);
    }
    if let Some(value) = restriction.model_processing {
        base.model_processing = base.model_processing.min(value);
    }
    if let Some(value) = restriction.personal_agent_access {
        base.personal_agent_access = base.personal_agent_access.min(value);
    }
    if let Some(value) = restriction.memory_promotion {
        base.memory_promotion = base.memory_promotion.min(value);
    }
    if let Some(value) = restriction.external_egress {
        base.external_egress = base.external_egress.min(value);
    }
    if !restriction.approved_destinations.is_empty() {
        let ceiling = restriction
            .approved_destinations
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        base.approved_destinations
            .retain(|destination| ceiling.contains(destination));
    }
    base.approved_destinations.sort();
    base.approved_destinations.dedup();
    if base.external_egress == AppExternalEgress::Denied || base.approved_destinations.is_empty() {
        base.external_egress = AppExternalEgress::Denied;
        base.approved_destinations.clear();
    }
    base
}

#[derive(Debug, Error)]
pub enum AppSchemaCompilerError {
    #[error("app schema contract is invalid: {0}")]
    Contract(#[from] AppContractError),
    #[error("app query schema is invalid: {0}")]
    Query(#[from] AppQuerySemanticsError),
    #[error("failed to encode the canonical app schema: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("compiled entity schema does not match the immutable package revision")]
    EntitySchemaDigestMismatch,
    #[error("reference targets missing entity `{0}`")]
    MissingReferenceTarget(String),
    #[error("compiled app schema is inconsistent: {0}")]
    InvalidCompiledSchema(&'static str),
    #[error("persisted app record does not match its active schema: {0}")]
    InvalidRecordPayload(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::apps::{
        manifest::parse_app_manifest_yaml,
        models::{
            AppDataClassification, AppModelProcessing, AppOrderDirection, AppProtocolVersion,
            AppQueryOrder, AppQueryRequest,
        },
        query_semantics::validate_typed_query,
        records::{AppMemoryPromotion, AppPersonalAgentAccess},
    };

    const MANIFEST_PREFIX: &str = r#"
name: reading-list
version: 0.1.0
description: A private reading list.
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
      classification_floor: personal
      model_processing: local_only
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
"#;

    const MANIFEST_SUFFIX: &str = r#"
  views:
    items:
      entity: item
      kind: table
      route: /
      columns: [title, status]
  workflows: {}
  actions: {}
  resources:
    per_run:
      max_tokens: 1000
      max_cost_usd: 0.25
      max_active_seconds: 60
    monthly:
      max_tokens: 10000
      max_cost_usd: 5.00
    storage:
      max_records: 10000
      max_bytes: 10485760
  dependencies:
    procedure_skills: []
    capabilities: []
  assets: []
"#;

    fn manifest(fields: &str) -> AppPackageManifest {
        parse_app_manifest_yaml(
            format!("{MANIFEST_PREFIX}{fields}{MANIFEST_SUFFIX}").as_bytes(),
            &super::super::manifest::AppPackageLimits::default(),
        )
        .unwrap()
        .manifest()
        .clone()
    }

    fn policy() -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn compile(manifest: &AppPackageManifest) -> CompiledAppSchema {
        let digest = canonical_entity_schema_digest(manifest).unwrap();
        compile_app_schema(
            manifest,
            AppInstallationId::parse("install_1").unwrap(),
            AppReference::parse("package-revision:one").unwrap(),
            AppRevision::new(1).unwrap(),
            policy(),
            AppSchemaCompatibility::Initial,
            None,
            &digest,
            Utc.with_ymd_and_hms(2026, 8, 16, 0, 0, 0).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn package_schema_digest_is_canonical_across_mapping_order() {
        let first = manifest(
            "    item:\n      fields:\n        title: { type: text, required: true }\n        \
             details: { type: markdown, nullable: true }\n        status: { type: enum, values: \
             [open, done], required: true }\n",
        );
        let reordered = manifest(
            "    item:\n      fields:\n        status: { type: enum, values: [open, done], \
             required: true }\n        details: { type: markdown, nullable: true }\n        \
             title: { type: text, required: true }\n",
        );

        assert_eq!(
            canonical_entity_schema_digest(&first).unwrap(),
            canonical_entity_schema_digest(&reordered).unwrap()
        );
    }

    #[test]
    fn compiler_binds_package_digest_and_only_indexes_declared_view_fields() {
        let manifest = manifest(
            "    item:\n      fields:\n        title: { type: text, required: true }\n        \
             details: { type: markdown, nullable: true }\n        status: { type: enum, values: \
             [open, done], required: true }\n",
        );
        let compiled = compile(&manifest);
        assert_eq!(
            source_entity_schema_digest_from_revision(compiled.revision()).unwrap(),
            *compiled.entity_schema_digest()
        );
        let rebuilt = query_schemas_from_revision(compiled.revision()).unwrap();
        assert_eq!(rebuilt.len(), 1);

        let mut query = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: None,
            order: vec![AppQueryOrder {
                field: AppFieldPath::parse("status").unwrap(),
                direction: AppOrderDirection::Ascending,
            }],
            cursor: None,
            limit: 25,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("surface").unwrap(),
        };
        validate_typed_query(
            &query,
            compiled
                .query_schema(&AppName::parse("item").unwrap())
                .unwrap(),
        )
        .unwrap();

        query.order[0].field = AppFieldPath::parse("details").unwrap();
        assert!(matches!(
            validate_typed_query(
                &query,
                compiled.query_schema(&AppName::parse("item").unwrap()).unwrap(),
            ),
            Err(AppQuerySemanticsError::UnstableOrderField(field)) if field == "details"
        ));
    }

    #[test]
    fn compiler_rejects_a_package_schema_digest_mismatch() {
        let manifest = manifest(
            "    item:\n      fields:\n        title: { type: text, required: true }\n        \
             status: { type: enum, values: [open, done], required: true }\n",
        );
        let result = compile_app_schema(
            &manifest,
            AppInstallationId::parse("install_1").unwrap(),
            AppReference::parse("package-revision:one").unwrap(),
            AppRevision::new(1).unwrap(),
            policy(),
            AppSchemaCompatibility::Initial,
            None,
            &AppDigest::blake3(b"wrong"),
            Utc.with_ymd_and_hms(2026, 8, 16, 0, 0, 0).unwrap(),
        );
        assert!(matches!(
            result,
            Err(AppSchemaCompilerError::EntitySchemaDigestMismatch)
        ));
    }

    #[test]
    fn persisted_query_contract_rejects_tampered_index_metadata() {
        let manifest = manifest(
            "    item:\n      fields:\n        title: { type: text, required: true }\n        \
             status: { type: enum, values: [open, done], required: true }\n",
        );
        let mut revision = compile(&manifest).into_revision();
        revision.compiled_index_plan["entities"]["item"]["sortable_fields"] =
            serde_json::json!(["unknown"]);

        assert!(matches!(
            query_schemas_from_revision(&revision),
            Err(AppSchemaCompilerError::InvalidCompiledSchema(_))
        ));
    }

    #[test]
    fn runtime_schema_revalidates_required_unknown_enum_and_scalar_values() {
        let manifest = manifest(
            "    item:\n      fields:\n        title: { type: text, required: true }\n        \
             status: { type: enum, values: [open, done], required: true }\n        estimate: { \
             type: decimal, nullable: true }\n        due_at: { type: timestamp, nullable: true \
             }\n",
        );
        let revision = compile(&manifest).into_revision();
        let runtime_contracts = runtime_contracts_from_revision(&revision).unwrap();
        let runtime = runtime_contracts
            .get(&AppName::parse("item").unwrap())
            .unwrap();

        runtime
            .validate_payload(&json!({
                "title": "Read",
                "status": "open",
                "estimate": 1.25,
                "due_at": "2026-08-16T10:00:00Z"
            }))
            .unwrap();
        for invalid in [
            json!({"status": "open"}),
            json!({"title": "Read", "status": "later"}),
            json!({"title": "Read", "status": "open", "estimate": "1.25"}),
            json!({"title": "Read", "status": "open", "unknown": true}),
            json!({"title": "Read", "status": "open", "due_at": "tomorrow"}),
        ] {
            assert!(matches!(
                runtime.validate_payload(&invalid),
                Err(AppSchemaCompilerError::InvalidRecordPayload(_))
            ));
        }
    }

    #[test]
    fn runtime_contract_cache_reuses_only_the_exact_validated_schema_documents() {
        let revision = compile(&manifest(
            "    item:\n      fields:\n        title: { type: text, required: true }\n        \
             status: { type: enum, values: [open, done], required: true }\n",
        ))
        .into_revision();
        let first = runtime_contracts_from_revision(&revision).unwrap();
        let second = runtime_contracts_from_revision(&revision).unwrap();
        assert!(Arc::ptr_eq(&first, &second));

        let mut tampered = revision.clone();
        tampered.compiled_index_plan["entities"]["item"]["sortable_fields"] =
            serde_json::json!(["unknown"]);
        assert!(matches!(
            runtime_contracts_from_revision(&tampered),
            Err(AppSchemaCompilerError::InvalidCompiledSchema(_))
        ));

        let mut validation_tampered = revision;
        validation_tampered.compiled_validation_schema = serde_json::json!({});
        assert!(matches!(
            runtime_contracts_from_revision(&validation_tampered),
            Err(AppSchemaCompilerError::InvalidCompiledSchema(_))
        ));
    }
}
