//! Phase 6 declarative schema migration compiler and dry-run.
//!
//! V1 operations are total and publisher-SQL-free: add a nullable or
//! defaulted field, rename, widen a number, map or widen an enum, and
//! retire a field. A plan that cannot represent every existing record
//! fails before any active pointer changes.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    models::{
        AppContractLimits, AppDigest, AppFieldPath, AppModelProcessing, AppName, AppReference,
    },
    query_semantics::AppQueryScalarKind,
    records::{AppDataHandlingPolicy, AppSchemaCompatibility},
    value_mapping::AppRegisteredScalarConversion,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppMigrationOperation {
    /// Explicit owner-review operation for records that predate a remote
    /// processing grant. Never inferred from a package or grant change.
    EnableRemoteProcessingForExistingRecords { entity: AppName },
    AddField {
        entity: AppName,
        field: AppFieldPath,
        scalar: AppQueryScalarKind,
        nullable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Value>,
    },
    RenameField {
        entity: AppName,
        from: AppFieldPath,
        to: AppFieldPath,
    },
    WidenNumeric {
        entity: AppName,
        field: AppFieldPath,
        conversion: AppRegisteredScalarConversion,
    },
    MapEnum {
        entity: AppName,
        field: AppFieldPath,
        values: BTreeMap<AppName, AppName>,
    },
    WidenEnum {
        entity: AppName,
        field: AppFieldPath,
        added: BTreeSet<AppName>,
    },
    RetireField {
        entity: AppName,
        field: AppFieldPath,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppCompiledMigrationPlan {
    plan_ref: AppReference,
    source_schema_digest: AppDigest,
    destination_schema_digest: AppDigest,
    operations: Vec<AppMigrationOperation>,
    compatibility: AppSchemaCompatibility,
    plan_digest: AppDigest,
}

impl AppCompiledMigrationPlan {
    pub fn record_processing_backup_required(&self, examined_records: u64) -> bool {
        examined_records > 0 && self.enables_remote_processing()
    }

    pub fn enables_remote_processing(&self) -> bool {
        self.operations
            .iter()
            .any(AppMigrationOperation::enables_remote_processing)
    }
    pub fn plan_ref(&self) -> &AppReference {
        &self.plan_ref
    }

    pub fn compatibility(&self) -> AppSchemaCompatibility {
        self.compatibility
    }

    pub fn plan_digest(&self) -> &AppDigest {
        &self.plan_digest
    }

    pub fn operations(&self) -> &[AppMigrationOperation] {
        &self.operations
    }

    pub fn source_schema_digest(&self) -> &AppDigest {
        &self.source_schema_digest
    }

    pub fn destination_schema_digest(&self) -> &AppDigest {
        &self.destination_schema_digest
    }
}

impl AppMigrationOperation {
    pub fn enables_remote_processing(&self) -> bool {
        matches!(self, Self::EnableRemoteProcessingForExistingRecords { .. })
    }
}

/// This pure transformation is used only by the owner update coordinator.
/// Record payloads and every other handling-policy dimension stay unchanged.
/// `none` is not released by this operation; field-level pins remain in the
/// separately reviewed compiled schema and continue to govern reads.
pub fn apply_migration_to_handling_policy(
    plan: &AppCompiledMigrationPlan,
    entity: &AppName,
    policy: &AppDataHandlingPolicy,
) -> AppDataHandlingPolicy {
    let mut next = policy.clone();
    if next.model_processing == AppModelProcessing::LocalOnly
        && plan.operations.iter().any(|operation| matches!(operation,
            AppMigrationOperation::EnableRemoteProcessingForExistingRecords { entity: target } if target == entity))
    {
        next.model_processing = AppModelProcessing::RemoteAllowed;
    }
    next
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMigrationRecordFailure {
    pub entity: AppName,
    pub record_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppMigrationDryRun {
    pub plan_digest: AppDigest,
    pub examined: u64,
    pub representable: u64,
    pub failures: Vec<AppMigrationRecordFailure>,
}

impl AppMigrationDryRun {
    pub fn succeeded(&self) -> bool {
        self.failures.is_empty()
    }
}

pub const APP_MIGRATION_TAIL_CHUNK: usize = 256;
pub const APP_MIGRATION_TAIL_RECORD_CEILING: u64 = 100_000;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMigrationTailProgress {
    pub plan_digest: AppDigest,
    pub examined: u64,
    pub migrated: u64,
    pub failed: u64,
    pub next_offset: u64,
    pub complete: bool,
}

/// Apply one bounded chunk of a staged tail. Crash recovery resumes from
/// `offset`. The 100,000-record ceiling is a hard fail-closed bound.
pub fn catch_up_migration_tail(
    plan: &AppCompiledMigrationPlan,
    records: &[(AppName, String, Value)],
    offset: u64,
    chunk: usize,
) -> Result<(AppMigrationTailProgress, Vec<(AppName, String, Value)>), AppMigrationError> {
    let total = u64::try_from(records.len()).unwrap_or(u64::MAX);
    if total > APP_MIGRATION_TAIL_RECORD_CEILING {
        return Err(AppMigrationError::TailCeilingExceeded);
    }
    if offset > total {
        return Err(AppMigrationError::TailOffsetPastEnd);
    }
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let take = chunk.max(1).min(APP_MIGRATION_TAIL_CHUNK);
    let slice = records.get(start..).unwrap_or(&[]).iter().take(take);
    let mut migrated = 0u64;
    let mut failed = 0u64;
    let mut rewritten = Vec::new();
    let mut examined = 0u64;
    for (entity, record_id, payload) in slice {
        examined += 1;
        match apply_migration_to_payload(plan, entity, payload) {
            Ok(next) => {
                migrated += 1;
                rewritten.push((entity.clone(), record_id.clone(), next));
            },
            Err(_) => failed += 1,
        }
    }
    let next_offset = offset.saturating_add(examined);
    Ok((
        AppMigrationTailProgress {
            plan_digest: plan.plan_digest.clone(),
            examined,
            migrated,
            failed,
            next_offset,
            complete: next_offset >= total,
        },
        rewritten,
    ))
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppMigrationError {
    #[error("app migration operation limit exceeded (limit={limit})")]
    OperationLimit { limit: usize },
    #[error("app migration is empty")]
    EmptyPlan,
    #[error("app migration cannot add required field `{field}` without a default")]
    RequiredFieldNeedsDefault { field: String },
    #[error("app migration rename `{from}` -> `{to}` is invalid")]
    InvalidRename { from: String, to: String },
    #[error("app migration numeric widening is not registered")]
    UnregisteredWidening,
    #[error("app migration enum map is incomplete or targets an unknown value")]
    InvalidEnumMapping,
    #[error("app migration graph contains a cycle or conflicting field identity")]
    ConflictingFieldIdentity,
    #[error("app migration cannot represent record `{record_id}`: {reason}")]
    UnrepresentableRecord { record_id: String, reason: String },
    #[error("app migration encoding failed: {0}")]
    Encoding(String),
    #[error("invalid app migration identity: {0}")]
    InvalidIdentity(String),
    #[error("app migration tail exceeds the 100,000-record ceiling")]
    TailCeilingExceeded,
    #[error("app migration tail offset is past the remaining records")]
    TailOffsetPastEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMigrationField {
    pub kind: AppQueryScalarKind,
    pub required: bool,
    pub nullable: bool,
    pub enum_values: BTreeSet<AppName>,
}

pub fn infer_schema_compatibility(
    source: &BTreeMap<AppName, BTreeMap<AppFieldPath, AppMigrationField>>,
    destination: &BTreeMap<AppName, BTreeMap<AppFieldPath, AppMigrationField>>,
) -> AppSchemaCompatibility {
    if source == destination
        || (source.is_empty() && destination.is_empty())
        || source.keys().eq(destination.keys())
            && source.iter().all(|(entity, fields)| {
                destination
                    .get(entity)
                    .is_some_and(|next| field_maps_equal(fields, next))
            })
    {
        return AppSchemaCompatibility::Compatible;
    }
    match compile_inferred_migration(
        AppReference::parse("plan:infer").unwrap_or_else(|_| unreachable_plan_ref()),
        AppDigest::blake3(b"source"),
        AppDigest::blake3(b"destination"),
        source,
        destination,
    ) {
        Ok(plan) => plan.compatibility,
        Err(_) => AppSchemaCompatibility::Incompatible,
    }
}

pub fn compile_migration_plan(
    plan_ref: AppReference,
    source_schema_digest: AppDigest,
    destination_schema_digest: AppDigest,
    operations: Vec<AppMigrationOperation>,
) -> Result<AppCompiledMigrationPlan, AppMigrationError> {
    let limits = AppContractLimits::default();
    if operations.is_empty() {
        return Err(AppMigrationError::EmptyPlan);
    }
    if operations.len() > limits.max_collection_items() {
        return Err(AppMigrationError::OperationLimit {
            limit: limits.max_collection_items(),
        });
    }
    validate_operations(&operations)?;
    let compatibility = if operations
        .iter()
        .all(|operation| matches!(operation, AppMigrationOperation::AddField { .. }))
    {
        AppSchemaCompatibility::Compatible
    } else {
        AppSchemaCompatibility::MigrationRequired
    };
    let plan_digest = digest_plan(
        &plan_ref,
        &source_schema_digest,
        &destination_schema_digest,
        &operations,
    )?;
    Ok(AppCompiledMigrationPlan {
        plan_ref,
        source_schema_digest,
        destination_schema_digest,
        operations,
        compatibility,
        plan_digest,
    })
}

pub fn compile_inferred_migration(
    plan_ref: AppReference,
    source_schema_digest: AppDigest,
    destination_schema_digest: AppDigest,
    source: &BTreeMap<AppName, BTreeMap<AppFieldPath, AppMigrationField>>,
    destination: &BTreeMap<AppName, BTreeMap<AppFieldPath, AppMigrationField>>,
) -> Result<AppCompiledMigrationPlan, AppMigrationError> {
    let mut operations = Vec::new();
    let mut entities = BTreeSet::new();
    entities.extend(source.keys().cloned());
    entities.extend(destination.keys().cloned());
    for entity in entities {
        let empty = BTreeMap::new();
        let from = source.get(&entity).unwrap_or(&empty);
        let to = destination.get(&entity).unwrap_or(&empty);
        for (field, dest) in to {
            match from.get(field) {
                None => {
                    if dest.required && !dest.nullable {
                        return Err(AppMigrationError::RequiredFieldNeedsDefault {
                            field: field.to_string(),
                        });
                    }
                    operations.push(AppMigrationOperation::AddField {
                        entity: entity.clone(),
                        field: field.clone(),
                        scalar: dest.kind,
                        nullable: dest.nullable || !dest.required,
                        default: None,
                    });
                },
                Some(source_field) => {
                    operations.extend(infer_field_change(&entity, field, source_field, dest)?);
                },
            }
        }
        for field in from.keys() {
            if !to.contains_key(field) {
                operations.push(AppMigrationOperation::RetireField {
                    entity: entity.clone(),
                    field: field.clone(),
                });
            }
        }
    }
    if operations.is_empty() {
        return Ok(AppCompiledMigrationPlan {
            plan_ref,
            source_schema_digest,
            destination_schema_digest,
            operations: Vec::new(),
            compatibility: AppSchemaCompatibility::Compatible,
            plan_digest: AppDigest::blake3(b"compatible-no-op"),
        });
    }
    compile_migration_plan(
        plan_ref,
        source_schema_digest,
        destination_schema_digest,
        operations,
    )
}

pub fn dry_run_migration(
    plan: &AppCompiledMigrationPlan,
    records: &[(AppName, String, Value)],
) -> AppMigrationDryRun {
    let mut failures = Vec::new();
    let mut representable = 0u64;
    for (entity, record_id, payload) in records {
        match apply_migration_to_payload(plan, entity, payload) {
            Ok(_) => representable += 1,
            Err(error) => failures.push(AppMigrationRecordFailure {
                entity: entity.clone(),
                record_id: record_id.clone(),
                reason: error.to_string(),
            }),
        }
    }
    AppMigrationDryRun {
        plan_digest: plan.plan_digest.clone(),
        examined: u64::try_from(records.len()).unwrap_or(u64::MAX),
        representable,
        failures,
    }
}

pub fn apply_migration_to_payload(
    plan: &AppCompiledMigrationPlan,
    entity: &AppName,
    payload: &Value,
) -> Result<Value, AppMigrationError> {
    let mut object =
        payload
            .as_object()
            .cloned()
            .ok_or_else(|| AppMigrationError::UnrepresentableRecord {
                record_id: entity.to_string(),
                reason: "payload is not an object".to_owned(),
            })?;
    for operation in &plan.operations {
        apply_operation(entity, operation, &mut object)?;
    }
    Ok(Value::Object(object))
}

fn apply_operation(
    entity: &AppName,
    operation: &AppMigrationOperation,
    object: &mut serde_json::Map<String, Value>,
) -> Result<(), AppMigrationError> {
    match operation {
        AppMigrationOperation::AddField {
            entity: target,
            field,
            default,
            nullable,
            ..
        } if target == entity => {
            if object.contains_key(field.as_str()) {
                return Ok(());
            }
            if let Some(default) = default {
                object.insert(field.as_str().to_owned(), default.clone());
            } else if *nullable {
                object.insert(field.as_str().to_owned(), Value::Null);
            }
            Ok(())
        },
        AppMigrationOperation::RenameField {
            entity: target,
            from,
            to,
        } if target == entity => match object.remove(from.as_str()) {
            Some(value) if !object.contains_key(to.as_str()) => {
                object.insert(to.as_str().to_owned(), value);
                Ok(())
            },
            None => Ok(()),
            Some(_) => Err(AppMigrationError::InvalidRename {
                from: from.to_string(),
                to: to.to_string(),
            }),
        },
        AppMigrationOperation::WidenNumeric {
            entity: target,
            field,
            conversion,
        } if target == entity => {
            if let Some(value) = object.get_mut(field.as_str()) {
                *value = widen_numeric(value, *conversion)?;
            }
            Ok(())
        },
        AppMigrationOperation::MapEnum {
            entity: target,
            field,
            values,
        } if target == entity => {
            if let Some(value) = object.get_mut(field.as_str()) {
                if value.is_null() {
                    return Ok(());
                }
                let current = value
                    .as_str()
                    .and_then(|value| AppName::parse(value).ok())
                    .ok_or_else(|| AppMigrationError::UnrepresentableRecord {
                        record_id: field.to_string(),
                        reason: "enum value is not a name".to_owned(),
                    })?;
                let mapped = values.get(&current).ok_or_else(|| {
                    AppMigrationError::UnrepresentableRecord {
                        record_id: field.to_string(),
                        reason: format!("unmapped enum value `{current}`"),
                    }
                })?;
                *value = Value::String(mapped.to_string());
            }
            Ok(())
        },
        AppMigrationOperation::WidenEnum { .. } | AppMigrationOperation::RetireField { .. } => {
            if let AppMigrationOperation::RetireField {
                entity: target,
                field,
            } = operation
            {
                if target == entity {
                    object.remove(field.as_str());
                }
            }
            Ok(())
        },
        _ => Ok(()),
    }
}

fn widen_numeric(
    value: &Value,
    conversion: AppRegisteredScalarConversion,
) -> Result<Value, AppMigrationError> {
    match conversion {
        AppRegisteredScalarConversion::IntegerToDecimal => value
            .as_i64()
            .map(Value::from)
            .or_else(|| value.as_u64().map(Value::from))
            .ok_or_else(|| AppMigrationError::UnrepresentableRecord {
                record_id: "numeric".to_owned(),
                reason: "value is not an integer".to_owned(),
            }),
        _ => Err(AppMigrationError::UnregisteredWidening),
    }
}

fn infer_field_change(
    entity: &AppName,
    field: &AppFieldPath,
    source: &AppMigrationField,
    dest: &AppMigrationField,
) -> Result<Vec<AppMigrationOperation>, AppMigrationError> {
    if source.kind == dest.kind && source.enum_values == dest.enum_values {
        return Ok(Vec::new());
    }
    if source.kind == AppQueryScalarKind::Integer && dest.kind == AppQueryScalarKind::Decimal {
        return Ok(vec![AppMigrationOperation::WidenNumeric {
            entity: entity.clone(),
            field: field.clone(),
            conversion: AppRegisteredScalarConversion::IntegerToDecimal,
        }]);
    }
    if source.kind == AppQueryScalarKind::Enum && dest.kind == AppQueryScalarKind::Enum {
        if source.enum_values.is_subset(&dest.enum_values) {
            return Ok(vec![AppMigrationOperation::WidenEnum {
                entity: entity.clone(),
                field: field.clone(),
                added: dest
                    .enum_values
                    .difference(&source.enum_values)
                    .cloned()
                    .collect(),
            }]);
        }
        return Err(AppMigrationError::InvalidEnumMapping);
    }
    if source.kind != dest.kind {
        return Err(AppMigrationError::UnregisteredWidening);
    }
    Ok(Vec::new())
}

fn validate_operations(operations: &[AppMigrationOperation]) -> Result<(), AppMigrationError> {
    let mut identities = BTreeSet::new();
    let mut policy_entities = BTreeSet::new();
    for operation in operations {
        match operation {
            AppMigrationOperation::EnableRemoteProcessingForExistingRecords { entity } => {
                if !policy_entities.insert(entity) {
                    return Err(AppMigrationError::ConflictingFieldIdentity);
                }
            },
            AppMigrationOperation::AddField {
                entity,
                field,
                nullable,
                default,
                ..
            } => {
                if !nullable && default.is_none() {
                    return Err(AppMigrationError::RequiredFieldNeedsDefault {
                        field: field.to_string(),
                    });
                }
                if !identities.insert((entity.as_str().to_owned(), field.as_str().to_owned())) {
                    return Err(AppMigrationError::ConflictingFieldIdentity);
                }
            },
            AppMigrationOperation::RenameField { entity, from, to } => {
                if from == to {
                    return Err(AppMigrationError::InvalidRename {
                        from: from.to_string(),
                        to: to.to_string(),
                    });
                }
                identities.insert((entity.as_str().to_owned(), from.as_str().to_owned()));
                identities.insert((entity.as_str().to_owned(), to.as_str().to_owned()));
            },
            AppMigrationOperation::WidenNumeric { conversion, .. } => {
                if *conversion != AppRegisteredScalarConversion::IntegerToDecimal {
                    return Err(AppMigrationError::UnregisteredWidening);
                }
            },
            AppMigrationOperation::MapEnum { values, .. } => {
                if values.is_empty() {
                    return Err(AppMigrationError::InvalidEnumMapping);
                }
            },
            AppMigrationOperation::WidenEnum { .. } | AppMigrationOperation::RetireField { .. } => {
            },
        }
    }
    Ok(())
}

fn field_maps_equal(
    left: &BTreeMap<AppFieldPath, AppMigrationField>,
    right: &BTreeMap<AppFieldPath, AppMigrationField>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(key, value)| {
            right.get(key).is_some_and(|other| {
                value.kind == other.kind
                    && value.required == other.required
                    && value.nullable == other.nullable
                    && value.enum_values == other.enum_values
            })
        })
}

fn digest_plan(
    plan_ref: &AppReference,
    source: &AppDigest,
    destination: &AppDigest,
    operations: &[AppMigrationOperation],
) -> Result<AppDigest, AppMigrationError> {
    let value = serde_json::to_value(serde_json::json!({
        "plan_ref": plan_ref,
        "source": source,
        "destination": destination,
        "operations": operations,
    }))
    .map_err(|error| AppMigrationError::Encoding(error.to_string()))?;
    AppDigest::blake3_canonical_json(&value)
        .map_err(|error| AppMigrationError::Encoding(error.to_string()))
}

fn unreachable_plan_ref() -> AppReference {
    AppReference::parse("plan:compat").expect("static plan ref")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> AppName {
        AppName::parse(value).unwrap()
    }

    fn field(value: &str) -> AppFieldPath {
        AppFieldPath::parse(value).unwrap()
    }

    fn text(required: bool, nullable: bool) -> AppMigrationField {
        AppMigrationField {
            kind: AppQueryScalarKind::Text,
            required,
            nullable,
            enum_values: BTreeSet::new(),
        }
    }

    #[test]
    fn additive_nullable_field_is_compatible() {
        let mut source = BTreeMap::new();
        source.insert(
            name("item"),
            BTreeMap::from([(field("title"), text(true, false))]),
        );
        let mut destination = source.clone();
        destination
            .get_mut(&name("item"))
            .unwrap()
            .insert(field("note"), text(false, true));
        let plan = compile_inferred_migration(
            AppReference::parse("plan:add-note").unwrap(),
            AppDigest::blake3(b"s"),
            AppDigest::blake3(b"d"),
            &source,
            &destination,
        )
        .unwrap();
        assert_eq!(plan.compatibility, AppSchemaCompatibility::Compatible);
        let migrated =
            apply_migration_to_payload(&plan, &name("item"), &serde_json::json!({"title": "Asha"}))
                .unwrap();
        assert_eq!(migrated["note"], Value::Null);
    }

    #[test]
    fn rename_and_enum_map_are_representable_or_fail_before_switch() {
        let operations = vec![
            AppMigrationOperation::RenameField {
                entity: name("item"),
                from: field("title"),
                to: field("name"),
            },
            AppMigrationOperation::MapEnum {
                entity: name("item"),
                field: field("status"),
                values: BTreeMap::from([(name("open"), name("active"))]),
            },
        ];
        let plan = compile_migration_plan(
            AppReference::parse("plan:rename").unwrap(),
            AppDigest::blake3(b"s"),
            AppDigest::blake3(b"d"),
            operations,
        )
        .unwrap();
        assert_eq!(
            plan.compatibility,
            AppSchemaCompatibility::MigrationRequired
        );
        let ok = apply_migration_to_payload(
            &plan,
            &name("item"),
            &serde_json::json!({"title": "Asha", "status": "open"}),
        )
        .unwrap();
        assert_eq!(ok["name"], "Asha");
        assert_eq!(ok["status"], "active");
        let dry = dry_run_migration(
            &plan,
            &[(
                name("item"),
                "rec_1".to_owned(),
                serde_json::json!({"title": "Asha", "status": "closed"}),
            )],
        );
        assert!(!dry.succeeded());
    }

    #[test]
    fn required_field_without_default_is_incompatible() {
        let mut source = BTreeMap::new();
        source.insert(
            name("item"),
            BTreeMap::from([(field("title"), text(true, false))]),
        );
        let mut destination = source.clone();
        destination
            .get_mut(&name("item"))
            .unwrap()
            .insert(field("owner"), text(true, false));
        assert!(compile_inferred_migration(
            AppReference::parse("plan:bad").unwrap(),
            AppDigest::blake3(b"s"),
            AppDigest::blake3(b"d"),
            &source,
            &destination,
        )
        .is_err());
        assert_eq!(
            infer_schema_compatibility(&source, &destination),
            AppSchemaCompatibility::Incompatible
        );
    }

    #[test]
    fn tail_catch_up_is_chunked_and_resumes_from_offset() {
        let plan = compile_inferred_migration(
            AppReference::parse("plan:tail").unwrap(),
            AppDigest::blake3(b"s"),
            AppDigest::blake3(b"d"),
            &BTreeMap::from([(
                name("item"),
                BTreeMap::from([(field("title"), text(true, false))]),
            )]),
            &BTreeMap::from([(
                name("item"),
                BTreeMap::from([
                    (field("title"), text(true, false)),
                    (field("note"), text(false, true)),
                ]),
            )]),
        )
        .unwrap();
        let records = (0..5)
            .map(|index| {
                (
                    name("item"),
                    format!("rec_{index}"),
                    serde_json::json!({"title": format!("t{index}")}),
                )
            })
            .collect::<Vec<_>>();
        let (first, rewritten) = catch_up_migration_tail(&plan, &records, 0, 2).unwrap();
        assert_eq!(first.examined, 2);
        assert_eq!(first.next_offset, 2);
        assert!(!first.complete);
        assert_eq!(rewritten.len(), 2);
        assert_eq!(rewritten[0].2["note"], Value::Null);
        let (second, _) = catch_up_migration_tail(&plan, &records, first.next_offset, 8).unwrap();
        assert!(second.complete);
        assert_eq!(second.next_offset, 5);
        assert!(catch_up_migration_tail(&plan, &records, 9, 2).is_err());
    }
}
