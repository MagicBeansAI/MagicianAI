//! Bounded, declarative reconciliation for app-owned records. This module
//! plans a transaction; the Apps workflow/entity owners authorize and commit it.
//! There is no app identity, host-store access, script, or provider dispatch here.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::models::{
    AppExpectedRecordRevision, AppFieldPath, AppMutationOperation, AppName, AppRecordId,
    AppRecordProjection, AppReference,
};

const MAX_ROWS: u16 = 200;
// A reviewed per-run scan budget, independent of both query page size and app
// storage capacity. The runtime follows store cursors in bounded pages and
// still enforces its byte, checkpoint, deadline and transaction budgets.
const MAX_EXISTING_ROWS: u16 = u16::MAX;
const MAX_TARGETS: usize = 8;
const MAX_WRITES: usize = 256;

pub(crate) fn result_schema_source() -> super::recipe_ir::AppWorkflowValueSchemaSource {
    serde_json::from_str(include_str!("reconciliation-result-schema.json"))
        .expect("compiled reconciliation result schema")
}

pub(crate) fn unchanged_result() -> Value {
    unchanged_result_with_summary("App records already match the approved source.")
}

pub(crate) fn unchanged_result_with_summary(summary: &str) -> Value {
    serde_json::json!({
        "receipt_ref": null,
        "committed_record_revisions": [],
        "change_sequence": null,
        "summary": summary
    })
}

pub(crate) fn is_unchanged_result(value: &Value) -> bool {
    value.as_object().is_some_and(|fields| fields.len() == 4)
        && value.get("receipt_ref") == Some(&Value::Null)
        && value.get("change_sequence") == Some(&Value::Null)
        && value
            .get("committed_record_revisions")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        && value
            .get("summary")
            .and_then(Value::as_str)
            .is_some_and(|text| text.len() <= 4096)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppReconcileValue {
    Literal {
        value: Value,
    },
    Source {
        field: AppName,
        fallback: Option<Value>,
        /// Preserve a present null for a nullable target field. Missing source
        /// fields still require an explicit non-null fallback.
        #[serde(default, skip_serializing_if = "is_false")]
        allow_null: bool,
    },
    Timestamp,
    /// Serialize the entire admitted source row, without a lossy projection.
    SourceDocument {
        max_bytes: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReconcileSeed {
    pub key: String,
    pub record_id: AppRecordId,
    pub fields: BTreeMap<AppName, AppReconcileValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReconcileRetirement {
    pub discriminator: AppName,
    pub value: Value,
    pub patch: BTreeMap<AppName, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReconcileTarget {
    pub entity: AppName,
    /// Absent selects the primary source. Named sources are package-owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AppName>,
    pub key_field: AppName,
    pub source_key: Option<AppName>,
    pub fields: BTreeMap<AppName, AppReconcileValue>,
    /// Empty means create missing rows only. Existing fields outside this set
    /// are never changed, including timestamps or user-authored state.
    pub update_fields: BTreeSet<AppName>,
    pub seeds: Vec<AppReconcileSeed>,
    pub retirement: Option<AppReconcileRetirement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppReconcileSourceRows {
    Document,
    Page {
        rows_field: AppName,
        next_cursor_field: AppName,
        truncated_field: AppName,
    },
}

/// A bounded additional host read, selected solely by reviewed scalar input.
/// No source output can invent another read, select authority or create a loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReconcileSource {
    pub tool: AppName,
    pub action: AppName,
    pub primitive_ref: AppReference,
    pub action_ref: AppReference,
    pub parameters: BTreeMap<AppName, Value>,
    pub input_parameters: BTreeMap<AppName, AppName>,
    pub when_input_present: Option<AppName>,
    pub rows: AppReconcileSourceRows,
}

impl AppReconcileSource {
    pub fn enabled(&self, input: &Value) -> bool {
        self.when_input_present.as_ref().is_none_or(|field| {
            input
                .get(field.as_str())
                .is_some_and(|value| !value.is_null())
        })
    }

    pub fn bound_parameters(
        &self,
        input: &Value,
    ) -> Result<BTreeMap<AppName, Value>, AppReconciliationError> {
        resolve_parameters(&self.parameters, &self.input_parameters, input)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppReconciliation {
    pub tool: AppName,
    pub action: AppName,
    /// A closed first-page request, pinned in the reviewed package. Input
    /// cannot select a scope, provider, cursor, or mutation target.
    pub parameters: BTreeMap<AppName, Value>,
    /// Reviewed provider-parameter -> workflow-input-field bindings. Only
    /// these values may override the pinned request's defaults. Provider,
    /// action, scope and mutation targets always remain package/runtime owned.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input_parameters: BTreeMap<AppName, AppName>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<AppName, AppReconcileSource>,
    pub rows_field: AppName,
    pub next_cursor_field: AppName,
    pub truncated_field: AppName,
    pub max_source_rows: u16,
    pub max_existing_rows: u16,
    pub targets: Vec<AppReconcileTarget>,
}

/// A narrowing view derived from the compiled reconciliation declaration.
/// It is not a provider argument or caller-selected query. Row membership,
/// order, keys and completeness evidence survive unchanged; only fields that
/// no target consumes can be removed before durable continuation retention.
pub(crate) struct AppReconciliationSourceProjection {
    tool: AppName,
    action: AppName,
    rows: AppReconcileSourceRows,
    fields: Option<BTreeSet<String>>,
}

impl AppReconciliation {
    pub(crate) fn source_projection(
        &self,
        source: Option<&AppName>,
    ) -> Result<AppReconciliationSourceProjection, AppReconciliationError> {
        self.validate()?;
        let (tool, action, rows) = match source {
            Some(name) => {
                let read = self
                    .sources
                    .get(name)
                    .ok_or(AppReconciliationError("unknown source projection"))?;
                (read.tool.clone(), read.action.clone(), read.rows.clone())
            },
            None => (
                self.tool.clone(),
                self.action.clone(),
                AppReconcileSourceRows::Page {
                    rows_field: self.rows_field.clone(),
                    next_cursor_field: self.next_cursor_field.clone(),
                    truncated_field: self.truncated_field.clone(),
                },
            ),
        };
        let mut fields = BTreeSet::new();
        let mut complete_document = false;
        for target in self
            .targets
            .iter()
            .filter(|target| target.source.as_ref() == source)
        {
            if let Some(key) = &target.source_key {
                fields.insert(key.to_string());
            }
            for mapping in target.fields.values() {
                match mapping {
                    AppReconcileValue::Source { field, .. } => {
                        fields.insert(field.to_string());
                    },
                    AppReconcileValue::SourceDocument { .. } => complete_document = true,
                    AppReconcileValue::Literal { .. } | AppReconcileValue::Timestamp => {},
                }
            }
        }
        Ok(AppReconciliationSourceProjection {
            tool,
            action,
            rows,
            fields: (!complete_document).then_some(fields),
        })
    }
}

impl AppReconciliationSourceProjection {
    pub(crate) fn matches_target(&self, tool: &str, action: Option<&str>) -> bool {
        self.tool.as_str() == tool && Some(self.action.as_str()) == action
    }

    pub(crate) fn project(&self, mut value: Value) -> Value {
        let project_row = |row: &mut Value| {
            if let (Some(fields), Some(object)) = (&self.fields, row.as_object_mut()) {
                object.retain(|field, _| fields.contains(field));
            }
        };
        match &self.rows {
            AppReconcileSourceRows::Document => project_row(&mut value),
            AppReconcileSourceRows::Page {
                rows_field,
                next_cursor_field,
                truncated_field,
            } => {
                if let Some(page) = value.as_object_mut() {
                    page.retain(|field, _| {
                        field == rows_field.as_str()
                            || field == next_cursor_field.as_str()
                            || field == truncated_field.as_str()
                    });
                    if let Some(rows) = page
                        .get_mut(rows_field.as_str())
                        .and_then(Value::as_array_mut)
                    {
                        rows.iter_mut().for_each(project_row);
                    }
                }
            },
        }
        value
    }
}

#[derive(Debug, thiserror::Error)]
#[error("app reconciliation rejected: {0}")]
pub struct AppReconciliationError(pub &'static str);

fn scalar(value: &Value) -> bool {
    !value.is_array() && !value.is_object() && value.as_str().is_none_or(|text| text.len() <= 4096)
}

fn is_false(value: &bool) -> bool {
    !value
}

fn public_parameter(name: &AppName) -> bool {
    !matches!(
        name.as_str(),
        "principal"
            | "workspace"
            | "action"
            | "operation"
            | "method"
            | "provider"
            | "provider_name"
            | "tool"
            | "capability_name"
    ) && !name.as_str().starts_with('_')
}

pub(crate) fn valid_source_parameters(
    parameters: &BTreeMap<AppName, Value>,
    input_parameters: &BTreeMap<AppName, AppName>,
) -> bool {
    parameters
        .keys()
        .chain(input_parameters.keys())
        .collect::<BTreeSet<_>>()
        .len()
        <= 16
        && parameters
            .iter()
            .all(|(key, value)| public_parameter(key) && scalar(value))
        && input_parameters.keys().all(public_parameter)
}

fn resolve_parameters(
    parameters: &BTreeMap<AppName, Value>,
    input_parameters: &BTreeMap<AppName, AppName>,
    input: &Value,
) -> Result<BTreeMap<AppName, Value>, AppReconciliationError> {
    if !valid_source_parameters(parameters, input_parameters) {
        return Err(AppReconciliationError("invalid read parameters"));
    }
    let input = input
        .as_object()
        .ok_or(AppReconciliationError("input must be an object"))?;
    let mut parameters = parameters.clone();
    for (parameter, field) in input_parameters {
        if let Some(value) = input.get(field.as_str()).filter(|value| !value.is_null()) {
            if !scalar(value) {
                return Err(AppReconciliationError("non-scalar input parameter"));
            }
            parameters.insert(parameter.clone(), value.clone());
        }
    }
    Ok(parameters)
}

impl AppReconciliation {
    pub fn validate(&self) -> Result<(), AppReconciliationError> {
        if !(1..=MAX_ROWS).contains(&self.max_source_rows)
            || !(1..=MAX_EXISTING_ROWS).contains(&self.max_existing_rows)
            || self.targets.is_empty()
            || self.targets.len() > MAX_TARGETS
            || !valid_source_parameters(&self.parameters, &self.input_parameters)
            || self.sources.len() > 3
        {
            return Err(AppReconciliationError("invalid declaration bounds"));
        }
        for source in self.sources.values() {
            if !valid_source_parameters(&source.parameters, &source.input_parameters)
                || source.when_input_present.as_ref().is_some_and(|field| {
                    !source.input_parameters.values().any(|bound| bound == field)
                })
            {
                return Err(AppReconciliationError("invalid additional source"));
            }
        }
        let mut entities = BTreeSet::new();
        for target in &self.targets {
            if !entities.insert(&target.entity)
                || target
                    .source
                    .as_ref()
                    .is_some_and(|source| !self.sources.contains_key(source))
                || (target.fields.is_empty() && target.source_key.is_some())
                || target.fields.len() > 64
                || target.seeds.len() > 8
                || target.update_fields.contains(&target.key_field)
                || !target
                    .update_fields
                    .iter()
                    .all(|field| target.fields.contains_key(field))
            {
                return Err(AppReconciliationError("invalid target declaration"));
            }
            for field in target
                .fields
                .values()
                .chain(target.seeds.iter().flat_map(|seed| seed.fields.values()))
            {
                match field {
                    AppReconcileValue::Literal { value } if !scalar(value) => {
                        return Err(AppReconciliationError("non-scalar literal"))
                    },
                    AppReconcileValue::Source {
                        fallback: Some(value),
                        ..
                    } if !scalar(value) => {
                        return Err(AppReconciliationError("non-scalar fallback"))
                    },
                    AppReconcileValue::SourceDocument { max_bytes }
                        if !(1..=1_048_576).contains(max_bytes) =>
                    {
                        return Err(AppReconciliationError("invalid document byte bound"))
                    },
                    _ => {},
                }
            }
            if target.seeds.iter().any(|seed| {
                seed.key.is_empty()
                    || seed.key.len() > 255
                    || seed.fields.len() > 64
                    || seed.fields.values().any(|field| {
                        matches!(
                            field,
                            AppReconcileValue::Source { .. }
                                | AppReconcileValue::SourceDocument { .. }
                        )
                    })
            }) || target.retirement.as_ref().is_some_and(|rule| {
                !scalar(&rule.value)
                    || rule.patch.is_empty()
                    || rule.patch.len() > 64
                    || rule.patch.contains_key(&target.key_field)
                    || rule.patch.values().any(|value| !scalar(value))
            }) {
                return Err(AppReconciliationError("invalid seed or retirement"));
            }
            if target.source.as_ref().is_some_and(|name| {
                let source = &self.sources[name];
                (source.when_input_present.is_some()
                    || matches!(source.rows, AppReconcileSourceRows::Document))
                    && (target.retirement.is_some() || !target.seeds.is_empty())
            }) {
                return Err(AppReconciliationError(
                    "conditional/document source cannot retire or seed",
                ));
            }
        }
        if self.sources.keys().any(|name| {
            !self
                .targets
                .iter()
                .any(|target| target.source.as_ref() == Some(name))
        }) {
            return Err(AppReconciliationError("unused additional source"));
        }
        Ok(())
    }

    pub fn entities(&self) -> BTreeSet<AppName> {
        self.targets
            .iter()
            .map(|target| target.entity.clone())
            .collect()
    }

    /// Resolve only the declared scalar inputs. The workflow input must first
    /// pass its exact reviewed schema, and the host provider still proves the
    /// resulting argument bounds before any read.
    pub fn bound_parameters(
        &self,
        input: &Value,
    ) -> Result<BTreeMap<AppName, Value>, AppReconciliationError> {
        self.validate()?;
        resolve_parameters(&self.parameters, &self.input_parameters, input)
    }

    pub(crate) fn selected_fields(target: &AppReconcileTarget) -> BTreeSet<AppFieldPath> {
        std::iter::once(&target.key_field)
            .chain(target.fields.keys())
            .chain(target.retirement.iter().map(|rule| &rule.discriminator))
            .chain(target.retirement.iter().flat_map(|rule| rule.patch.keys()))
            .map(|field| AppFieldPath::parse(field.as_str()).expect("validated flat AppName"))
            .collect()
    }
}

/// Pure mutation proposals, without execution or commit authority. The Apps
/// owner must independently admit their exact targets, revisions and budget.
pub struct AppReconciliationPlan {
    pub operations: Vec<AppMutationOperation>,
    pub expected_record_revisions: Vec<AppExpectedRecordRevision>,
}

fn mapped_fields(
    fields: &BTreeMap<AppName, AppReconcileValue>,
    row: &Value,
    now: DateTime<Utc>,
) -> Result<Map<String, Value>, AppReconciliationError> {
    fields
        .iter()
        .map(|(name, mapping)| {
            let value = match mapping {
                AppReconcileValue::Literal { value } => value.clone(),
                AppReconcileValue::Timestamp => Value::String(now.to_rfc3339()),
                AppReconcileValue::SourceDocument { max_bytes } => {
                    let text = serde_json::to_string(row)
                        .map_err(|_| AppReconciliationError("invalid source document"))?;
                    if !row.is_object() || text.len() > *max_bytes as usize {
                        return Err(AppReconciliationError(
                            "source document exceeds reviewed byte bound",
                        ));
                    }
                    return Ok((name.to_string(), Value::String(text)));
                },
                AppReconcileValue::Source {
                    field,
                    fallback,
                    allow_null,
                } => row
                    .get(field.as_str())
                    .filter(|value| *allow_null || !value.is_null())
                    .cloned()
                    .or_else(|| fallback.clone())
                    .ok_or(AppReconciliationError("missing source field"))?,
            };
            if !scalar(&value) {
                return Err(AppReconciliationError("non-scalar source field"));
            }
            Ok((name.to_string(), value))
        })
        .collect()
}

/// Duplicate/oversized/malformed snapshots fail before any write. Updates
/// carry exact record revisions; creates use stable IDs and refuse collisions.
/// Retirement requires an explicitly complete source page. The caller must
/// prove that each existing-record page is complete as well.
pub fn plan_reconciliation(
    declaration: &AppReconciliation,
    source: &Value,
    existing: &BTreeMap<AppName, Vec<AppRecordProjection>>,
    now: DateTime<Utc>,
) -> Result<AppReconciliationPlan, AppReconciliationError> {
    plan_reconciliation_with_sources(
        declaration,
        &serde_json::json!({}),
        source,
        &BTreeMap::new(),
        existing,
        now,
    )
}

fn source_page<'a>(
    source: &'a Value,
    rows_field: &AppName,
    next_cursor_field: &AppName,
    truncated_field: &AppName,
    max_rows: u16,
    retiring: bool,
) -> Result<(&'a [Value], bool), AppReconciliationError> {
    let rows = source
        .get(rows_field.as_str())
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= usize::from(max_rows))
        .ok_or(AppReconciliationError("invalid source page"))?;
    let complete = match (
        source.get(next_cursor_field.as_str()),
        source.get(truncated_field.as_str()),
    ) {
        (Some(cursor), Some(Value::Bool(truncated))) => {
            if !cursor.is_null() && !cursor.is_string() {
                return Err(AppReconciliationError("invalid source cursor"));
            }
            cursor.is_null() && !*truncated
        },
        (None, None) if !retiring => false,
        _ => return Err(AppReconciliationError("missing completeness evidence")),
    };
    Ok((rows, complete))
}

/// All enabled reads must be present before any mutation is proposed. Skipped
/// optional reads leave their records untouched; failed reads cannot masquerade
/// as an empty page or erase a previously stored snapshot.
pub fn plan_reconciliation_with_sources(
    declaration: &AppReconciliation,
    input: &Value,
    source: &Value,
    sources: &BTreeMap<AppName, Value>,
    existing: &BTreeMap<AppName, Vec<AppRecordProjection>>,
    now: DateTime<Utc>,
) -> Result<AppReconciliationPlan, AppReconciliationError> {
    declaration.validate()?;
    if !input.is_object()
        || sources
            .keys()
            .any(|name| !declaration.sources.contains_key(name))
        || declaration
            .sources
            .iter()
            .any(|(name, read)| read.enabled(input) != sources.contains_key(name))
    {
        return Err(AppReconciliationError(
            "source set does not match reviewed input",
        ));
    }
    let primary = source_page(
        source,
        &declaration.rows_field,
        &declaration.next_cursor_field,
        &declaration.truncated_field,
        declaration.max_source_rows,
        declaration
            .targets
            .iter()
            .any(|target| target.source.is_none() && target.retirement.is_some()),
    )?;
    let mut plan = AppReconciliationPlan {
        operations: Vec::new(),
        expected_record_revisions: Vec::new(),
    };
    for target in &declaration.targets {
        let (rows, complete) = if let Some(name) = &target.source {
            let read = &declaration.sources[name];
            let Some(source) = sources.get(name) else {
                continue;
            };
            match &read.rows {
                AppReconcileSourceRows::Document if source.is_object() => {
                    (std::slice::from_ref(source), false)
                },
                AppReconcileSourceRows::Document => {
                    return Err(AppReconciliationError("source document must be an object"))
                },
                AppReconcileSourceRows::Page {
                    rows_field,
                    next_cursor_field,
                    truncated_field,
                } => source_page(
                    source,
                    rows_field,
                    next_cursor_field,
                    truncated_field,
                    declaration.max_source_rows,
                    target.retirement.is_some(),
                )?,
            }
        } else {
            primary
        };
        let stored = existing
            .get(&target.entity)
            .filter(|rows| rows.len() <= usize::from(declaration.max_existing_rows))
            .ok_or(AppReconciliationError("missing bounded store snapshot"))?;
        let mut by_key = BTreeMap::new();
        for record in stored {
            let key = record
                .fields
                .get(&AppFieldPath::parse(target.key_field.as_str()).unwrap())
                .and_then(Value::as_str)
                .ok_or(AppReconciliationError("invalid stored key"))?;
            if record.entity != target.entity || by_key.insert(key, record).is_some() {
                return Err(AppReconciliationError("duplicate stored key"));
            }
        }
        let mut seen = BTreeSet::new();
        let mut candidates = Vec::new();
        for seed in &target.seeds {
            if !seen.insert(seed.key.clone()) {
                return Err(AppReconciliationError("duplicate seed key"));
            }
            let mut fields = mapped_fields(&seed.fields, &Value::Null, now)?;
            fields.insert(
                target.key_field.to_string(),
                Value::String(seed.key.clone()),
            );
            candidates.push((seed.key.clone(), Some(seed.record_id.clone()), fields, true));
        }
        for row in rows.iter().filter(|_| target.source_key.is_some()) {
            let key = row
                .get(target.source_key.as_ref().unwrap().as_str())
                .and_then(Value::as_str)
                .filter(|key| !key.is_empty() && key.len() <= 255)
                .ok_or(AppReconciliationError("invalid source key"))?;
            if !seen.insert(key.to_owned()) {
                return Err(AppReconciliationError("duplicate source key"));
            }
            let mut fields = mapped_fields(&target.fields, row, now)?;
            fields.insert(target.key_field.to_string(), Value::String(key.to_owned()));
            candidates.push((key.to_owned(), None, fields, false));
        }
        for (key, explicit_id, fields, seed) in candidates {
            if let Some(record) = by_key.get(key.as_str()) {
                if seed {
                    continue;
                }
                let patch: Map<String, Value> = fields
                    .into_iter()
                    .filter(|(field, _)| {
                        target
                            .update_fields
                            .iter()
                            .any(|allowed| allowed.as_str() == field)
                    })
                    .collect();
                if patch.is_empty() {
                    continue;
                }
                plan.operations.push(AppMutationOperation::Update {
                    entity: target.entity.clone(),
                    record_id: record.record_id.clone(),
                    patch: Value::Object(patch),
                });
                plan.expected_record_revisions
                    .push(AppExpectedRecordRevision {
                        entity: target.entity.clone(),
                        record_id: record.record_id.clone(),
                        revision: record.record_revision,
                    });
            } else {
                let identity =
                    blake3::hash(format!("{}\0{key}", target.entity).as_bytes()).to_hex();
                plan.operations.push(AppMutationOperation::Create {
                    entity: target.entity.clone(),
                    temporary_id: AppName::parse(format!("row_{}", &identity[..24])).unwrap(),
                    record_id: Some(explicit_id.unwrap_or_else(|| {
                        AppRecordId::parse(format!("sync_{identity}")).unwrap()
                    })),
                    payload: Value::Object(fields),
                });
            }
        }
        if let Some(rule) = target.retirement.as_ref().filter(|_| complete) {
            let discriminator = AppFieldPath::parse(rule.discriminator.as_str()).unwrap();
            for (key, record) in by_key {
                if seen.contains(key) || record.fields.get(&discriminator) != Some(&rule.value) {
                    continue;
                }
                plan.operations.push(AppMutationOperation::Update {
                    entity: target.entity.clone(),
                    record_id: record.record_id.clone(),
                    patch: Value::Object(
                        rule.patch
                            .iter()
                            .map(|(key, value)| (key.to_string(), value.clone()))
                            .collect(),
                    ),
                });
                plan.expected_record_revisions
                    .push(AppExpectedRecordRevision {
                        entity: target.entity.clone(),
                        record_id: record.record_id.clone(),
                        revision: record.record_revision,
                    });
            }
        }
        if plan.operations.len() > MAX_WRITES {
            return Err(AppReconciliationError("write ceiling exceeded"));
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    #[test]
    fn bounded_page_summary_never_substitutes_for_a_mutation_receipt() {
        let output = super::unchanged_result_with_summary(
            "The source returned an empty bounded page; more pages remain.",
        );
        assert!(super::is_unchanged_result(&output));
        for (name, value) in [
            ("receipt_ref", serde_json::json!("receipt:invented")),
            (
                "change_sequence",
                serde_json::json!({"first": 1, "last": 1}),
            ),
            (
                "committed_record_revisions",
                serde_json::json!([{"revision": 1}]),
            ),
            ("summary", serde_json::json!("x".repeat(4097))),
            ("extra", serde_json::json!(true)),
        ] {
            let mut invalid = output.clone();
            invalid[name] = value;
            assert!(!super::is_unchanged_result(&invalid), "accepted {name}");
        }
    }

    use super::super::models::AppRevision;
    use super::*;
    use serde_json::json;

    fn declaration() -> AppReconciliation {
        // Deliberately different source and entity names from Town Square:
        // the planner is a shared data operation, with no app-specific branch.
        serde_json::from_value(json!({
            "tool":"thinking_maps_data", "action":"list_maps", "parameters":{"limit":20},
            "rows_field":"maps", "next_cursor_field":"next_cursor", "truncated_field":"scan_truncated",
            "max_source_rows":20, "max_existing_rows":20,
            "targets":[{
                "entity":"card", "key_field":"source_id", "source_key":"id",
                "fields":{
                    "title":{"kind":"source","field":"title","fallback":null},
                    "created_at":{"kind":"timestamp"},
                    "active":{"kind":"literal","value":true}
                },
                "update_fields":["title","active"], "seeds":[],
                "retirement":{"discriminator":"active","value":true,"patch":{"active":false}}
            }]
        })).unwrap()
    }

    fn record(key: &str) -> AppRecordProjection {
        AppRecordProjection {
            entity: AppName::parse("card").unwrap(),
            record_id: AppRecordId::parse(format!("old_{key}")).unwrap(),
            record_revision: AppRevision::new(7).unwrap(),
            fields: BTreeMap::from([
                (AppFieldPath::parse("source_id").unwrap(), json!(key)),
                (
                    AppFieldPath::parse("title").unwrap(),
                    json!("Original title"),
                ),
                (
                    AppFieldPath::parse("created_at").unwrap(),
                    json!("2020-01-01T00:00:00Z"),
                ),
                (AppFieldPath::parse("active").unwrap(), json!(true)),
            ]),
        }
    }

    fn page(cursor: Value) -> Value {
        json!({"maps":[{"id":"one","title":"Refreshed title"}], "next_cursor":cursor, "scan_truncated":false})
    }

    #[test]
    fn app_source_projection_preserves_mapping_and_completeness_failures() {
        let declaration = declaration();
        let projection = declaration.source_projection(None).unwrap();
        let existing = BTreeMap::from([(
            AppName::parse("card").unwrap(),
            vec![record("one"), record("retired")],
        )]);
        let now = Utc::now();
        for cursor in [Value::Null, json!("more"), json!(42)] {
            let mut source = page(cursor);
            source["maps"][0]["unused"] = json!("x".repeat(90_000));
            let narrowed = projection.project(source.clone());
            assert_eq!(narrowed["maps"].as_array().unwrap().len(), 1);
            assert_eq!(narrowed["next_cursor"], source["next_cursor"]);
            assert!(narrowed["maps"][0].get("unused").is_none());
            match (
                plan_reconciliation(&declaration, &source, &existing, now),
                plan_reconciliation(&declaration, &narrowed, &existing, now),
            ) {
                (Ok(before), Ok(after)) => {
                    assert_eq!(
                        serde_json::to_value(before.operations).unwrap(),
                        serde_json::to_value(after.operations).unwrap()
                    );
                    assert_eq!(
                        before.expected_record_revisions,
                        after.expected_record_revisions
                    );
                },
                (Err(before), Err(after)) => assert_eq!(before.0, after.0),
                _ => panic!("projection changed source validity"),
            }
        }
        let mut source = page(Value::Null);
        source.as_object_mut().unwrap().remove("scan_truncated");
        let narrowed = projection.project(source);
        assert!(narrowed.get("scan_truncated").is_none());
        assert!(plan_reconciliation(&declaration, &narrowed, &existing, now).is_err());
        let mut duplicate = page(Value::Null);
        duplicate["maps"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"one","title":"duplicate"}));
        assert!(
            plan_reconciliation(&declaration, &projection.project(duplicate), &existing, now)
                .is_err()
        );
    }

    #[test]
    fn app_source_projection_keeps_declared_documents_and_named_source_fields() {
        let mut declaration = declaration();
        declaration.targets[0].fields.insert(
            AppName::parse("document").unwrap(),
            AppReconcileValue::SourceDocument { max_bytes: 4096 },
        );
        let mut source = page(Value::Null);
        source["maps"][0]["nested"] = json!({"keep":[1,2,3]});
        assert_eq!(
            declaration
                .source_projection(None)
                .unwrap()
                .project(source.clone()),
            source
        );

        let named = AppName::parse("detail").unwrap();
        declaration.sources.insert(
            named.clone(),
            AppReconcileSource {
                tool: AppName::parse("thinking_maps_data").unwrap(),
                action: AppName::parse("read_map").unwrap(),
                primitive_ref: AppReference::parse("primitive:maps").unwrap(),
                action_ref: AppReference::parse("action:read_map").unwrap(),
                parameters: BTreeMap::new(),
                input_parameters: BTreeMap::new(),
                when_input_present: None,
                rows: AppReconcileSourceRows::Document,
            },
        );
        declaration.targets[0].source = Some(named.clone());
        declaration.targets[0].retirement = None;
        declaration.targets[0]
            .fields
            .remove(&AppName::parse("document").unwrap());
        let projection = declaration.source_projection(Some(&named)).unwrap();
        assert!(projection.matches_target("thinking_maps_data", Some("read_map")));
        assert!(!projection.matches_target("thinking_maps_data", Some("list_maps")));
        assert_eq!(
            projection.project(json!({"id":"one","title":null,"unused":"discard"})),
            json!({"id":"one","title":null})
        );
        assert!(declaration
            .source_projection(Some(&AppName::parse("unknown").unwrap()))
            .is_err());
    }

    #[test]
    fn preserves_unselected_fields_and_fences_each_update() {
        let existing = BTreeMap::from([(
            AppName::parse("card").unwrap(),
            vec![record("one"), record("retired")],
        )]);
        let plan =
            plan_reconciliation(&declaration(), &page(Value::Null), &existing, Utc::now()).unwrap();
        assert_eq!(plan.operations.len(), 2);
        let AppMutationOperation::Update { patch, .. } = &plan.operations[0] else {
            panic!("expected update")
        };
        assert_eq!(patch["title"], "Refreshed title");
        assert!(patch.get("created_at").is_none());
        assert!(patch.get("source_id").is_none());
        assert!(plan
            .expected_record_revisions
            .iter()
            .all(|fence| fence.revision.get() == 7));
    }

    #[test]
    fn partial_or_truncated_source_never_retires_missing_rows() {
        let existing = BTreeMap::from([(
            AppName::parse("card").unwrap(),
            vec![record("one"), record("retired")],
        )]);
        for source in [page(json!("next")), {
            let mut source = page(Value::Null);
            source["scan_truncated"] = json!(true);
            source
        }] {
            let plan = plan_reconciliation(&declaration(), &source, &existing, Utc::now()).unwrap();
            assert_eq!(plan.operations.len(), 1);
        }
        let mut missing = page(Value::Null);
        missing.as_object_mut().unwrap().remove("next_cursor");
        assert!(plan_reconciliation(&declaration(), &missing, &existing, Utc::now()).is_err());
    }

    #[test]
    fn duplicate_keys_and_overflow_fail_before_a_transaction_exists() {
        let existing = BTreeMap::from([(AppName::parse("card").unwrap(), vec![])]);
        let mut source = page(Value::Null);
        let duplicate = source["maps"][0].clone();
        source["maps"].as_array_mut().unwrap().push(duplicate);
        assert!(plan_reconciliation(&declaration(), &source, &existing, Utc::now()).is_err());
        let duplicates = BTreeMap::from([(
            AppName::parse("card").unwrap(),
            vec![record("one"), record("one")],
        )]);
        assert!(
            plan_reconciliation(&declaration(), &page(Value::Null), &duplicates, Utc::now())
                .is_err()
        );
        let mut bounded = declaration();
        bounded.max_source_rows = 1;
        assert!(plan_reconciliation(&bounded, &source, &existing, Utc::now()).is_err());
        let mut store_bound = declaration();
        store_bound.max_existing_rows = 1000;
        assert!(store_bound.validate().is_ok());
        store_bound.max_existing_rows = 0;
        assert!(store_bound.validate().is_err());
    }

    #[test]
    fn create_only_preserves_existing_state_and_creation_ids_are_stable() {
        let mut declaration = declaration();
        declaration.targets[0].update_fields.clear();
        declaration.targets[0].retirement = None;
        let existing = BTreeMap::from([(AppName::parse("card").unwrap(), vec![record("one")])]);
        assert!(
            plan_reconciliation(&declaration, &page(Value::Null), &existing, Utc::now())
                .unwrap()
                .operations
                .is_empty()
        );
        let empty = BTreeMap::from([(AppName::parse("card").unwrap(), vec![])]);
        let now = Utc::now();
        let first = plan_reconciliation(&declaration, &page(Value::Null), &empty, now).unwrap();
        let replay = plan_reconciliation(&declaration, &page(Value::Null), &empty, now).unwrap();
        assert_eq!(first.operations, replay.operations);
    }
}
