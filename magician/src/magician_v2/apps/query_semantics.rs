//! Exact Phase-0 query, predicate, ordering and cursor semantics.

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::models::{
    decode_bounded_json_value, AppComparisonOperator, AppContractLimits, AppDigest, AppFieldPath,
    AppInstallationId, AppName, AppPredicateNode, AppQueryRequest, AppRecordId, AppReference,
    AppRevision, ValidateAppContract,
};
use crate::magician_v2::json_traversal::canonical_json_bytes;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppQueryScalarKind {
    Text,
    Markdown,
    Integer,
    Decimal,
    Boolean,
    Timestamp,
    Enum,
    Reference,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppTextComparisonNormalization {
    UnicodeNfkcCaseSensitive,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppMissingValueSemantics {
    DistinctFromNullAndExcluded,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppQueryFieldContract {
    path: AppFieldPath,
    kind: AppQueryScalarKind,
    nullable: bool,
    indexed: bool,
    sortable: bool,
    enum_values: BTreeSet<AppName>,
}

impl AppQueryFieldContract {
    pub fn from_compiled_schema(
        path: AppFieldPath,
        kind: AppQueryScalarKind,
        nullable: bool,
        indexed: bool,
        sortable: bool,
    ) -> Self {
        Self {
            path,
            kind,
            nullable,
            indexed,
            sortable,
            enum_values: BTreeSet::new(),
        }
    }

    pub fn from_compiled_enum_schema(
        path: AppFieldPath,
        nullable: bool,
        indexed: bool,
        sortable: bool,
        enum_values: BTreeSet<AppName>,
    ) -> Result<Self, AppQuerySemanticsError> {
        let limit = AppContractLimits::default().max_collection_items();
        if enum_values.is_empty() || enum_values.len() > limit {
            return Err(AppQuerySemanticsError::InvalidEnumFieldContract { limit });
        }
        Ok(Self {
            path,
            kind: AppQueryScalarKind::Enum,
            nullable,
            indexed,
            sortable,
            enum_values,
        })
    }
}

/// Non-transport schema evidence produced by the manifest/schema compiler.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppQueryRelationContract {
    relation: AppName,
    selectable_fields: BTreeSet<AppFieldPath>,
    max_traversal_depth: u16,
}

impl AppQueryRelationContract {
    pub fn from_compiled_schema(
        relation: AppName,
        selectable_fields: BTreeSet<AppFieldPath>,
        max_traversal_depth: u16,
    ) -> Result<Self, AppQuerySemanticsError> {
        let limits = AppContractLimits::default();
        if selectable_fields.is_empty()
            || selectable_fields.len() > limits.max_collection_items()
            || max_traversal_depth == 0
            || usize::from(max_traversal_depth) > limits.max_predicate_depth()
        {
            return Err(AppQuerySemanticsError::InvalidRelationContract);
        }
        Ok(Self {
            relation,
            selectable_fields,
            max_traversal_depth,
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppTypedQuerySchema {
    fields: BTreeMap<AppFieldPath, AppQueryFieldContract>,
    relations: BTreeMap<AppName, AppQueryRelationContract>,
}

impl AppTypedQuerySchema {
    pub fn from_compiled_fields(
        fields: Vec<AppQueryFieldContract>,
    ) -> Result<Self, AppQuerySemanticsError> {
        Self::from_compiled_contracts(fields, Vec::new())
    }

    pub fn from_compiled_contracts(
        fields: Vec<AppQueryFieldContract>,
        relations: Vec<AppQueryRelationContract>,
    ) -> Result<Self, AppQuerySemanticsError> {
        let limits = AppContractLimits::default();
        if fields.is_empty()
            || fields.len() > limits.max_collection_items()
            || relations.len() > limits.max_collection_items()
        {
            return Err(AppQuerySemanticsError::FieldContractLimit {
                limit: limits.max_collection_items(),
            });
        }
        let mut indexed = BTreeMap::new();
        for field in fields {
            if (field.kind == AppQueryScalarKind::Enum) != !field.enum_values.is_empty() {
                return Err(AppQuerySemanticsError::InvalidEnumFieldContract {
                    limit: limits.max_collection_items(),
                });
            }
            if indexed.insert(field.path.clone(), field).is_some() {
                return Err(AppQuerySemanticsError::DuplicateFieldContract);
            }
        }
        let mut indexed_relations = BTreeMap::new();
        for relation in relations {
            if indexed_relations
                .insert(relation.relation.clone(), relation)
                .is_some()
            {
                return Err(AppQuerySemanticsError::DuplicateRelationContract);
            }
        }
        Ok(Self {
            fields: indexed,
            relations: indexed_relations,
        })
    }

    fn field(&self, path: &AppFieldPath) -> Result<&AppQueryFieldContract, AppQuerySemanticsError> {
        self.fields
            .get(path)
            .ok_or_else(|| AppQuerySemanticsError::UnknownField(path.to_string()))
    }
}

pub fn validate_typed_query(
    request: &AppQueryRequest,
    schema: &AppTypedQuerySchema,
) -> Result<(), AppQuerySemanticsError> {
    request.validate_app_contract(&AppContractLimits::default())?;
    for field in &request.select {
        schema.field(field)?;
    }
    for order in &request.order {
        let field = schema.field(&order.field)?;
        if !field.indexed || !field.sortable {
            return Err(AppQuerySemanticsError::UnstableOrderField(
                order.field.to_string(),
            ));
        }
    }
    if let Some(predicate) = &request.predicate {
        for node in &predicate.nodes {
            match node {
                AppPredicateNode::Compare {
                    field,
                    operator,
                    value,
                } => {
                    let contract = schema.field(field)?;
                    validate_operator(contract.kind, *operator)?;
                    validate_scalar_value(contract, value)?;
                },
                AppPredicateNode::In { field, values } => {
                    let contract = schema.field(field)?;
                    if values.is_empty() {
                        return Err(AppQuerySemanticsError::EmptyInPredicate);
                    }
                    for value in values {
                        validate_scalar_value(contract, value)?;
                    }
                },
                AppPredicateNode::IsNull { field, .. } => {
                    if !schema.field(field)?.nullable {
                        return Err(AppQuerySemanticsError::NonNullableNullPredicate(
                            field.to_string(),
                        ));
                    }
                },
                AppPredicateNode::All { .. }
                | AppPredicateNode::Any { .. }
                | AppPredicateNode::Not { .. } => {},
            }
        }
    }
    let mut expanded_relations = BTreeSet::new();
    for expansion in &request.relation_expansions {
        if !expanded_relations.insert(&expansion.relation) {
            return Err(AppQuerySemanticsError::DuplicateRelationExpansion(
                expansion.relation.to_string(),
            ));
        }
        let relation = schema.relations.get(&expansion.relation).ok_or_else(|| {
            AppQuerySemanticsError::UnknownRelation(expansion.relation.to_string())
        })?;
        if expansion.max_depth > relation.max_traversal_depth {
            return Err(AppQuerySemanticsError::RelationDepthExceeded {
                relation: expansion.relation.to_string(),
                limit: relation.max_traversal_depth,
            });
        }
        if let Some(field) = expansion
            .select
            .iter()
            .find(|field| !relation.selectable_fields.contains(*field))
        {
            return Err(AppQuerySemanticsError::UnknownRelationField {
                relation: expansion.relation.to_string(),
                field: field.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_operator(
    kind: AppQueryScalarKind,
    operator: AppComparisonOperator,
) -> Result<(), AppQuerySemanticsError> {
    let allowed = match operator {
        AppComparisonOperator::Equal | AppComparisonOperator::NotEqual => true,
        AppComparisonOperator::LessThan
        | AppComparisonOperator::LessThanOrEqual
        | AppComparisonOperator::GreaterThan
        | AppComparisonOperator::GreaterThanOrEqual => matches!(
            kind,
            AppQueryScalarKind::Integer
                | AppQueryScalarKind::Decimal
                | AppQueryScalarKind::Timestamp
        ),
        AppComparisonOperator::Contains | AppComparisonOperator::StartsWith => {
            matches!(
                kind,
                AppQueryScalarKind::Text | AppQueryScalarKind::Markdown
            )
        },
    };
    if !allowed {
        return Err(AppQuerySemanticsError::OperatorTypeMismatch { kind, operator });
    }
    Ok(())
}

fn validate_scalar_value(
    contract: &AppQueryFieldContract,
    value: &Value,
) -> Result<(), AppQuerySemanticsError> {
    if value.is_null() {
        return Err(AppQuerySemanticsError::NullRequiresIsNull);
    }
    let valid = match contract.kind {
        AppQueryScalarKind::Text | AppQueryScalarKind::Markdown => value.is_string(),
        AppQueryScalarKind::Enum => value.as_str().is_some_and(|value| {
            AppName::parse(value)
                .ok()
                .is_some_and(|value| contract.enum_values.contains(&value))
        }),
        AppQueryScalarKind::Reference => value
            .as_str()
            .is_some_and(|value| AppRecordId::parse(value).is_ok()),
        AppQueryScalarKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        AppQueryScalarKind::Decimal => {
            value.is_number() && Decimal::from_str(&value.to_string()).is_ok()
        },
        AppQueryScalarKind::Boolean => value.is_boolean(),
        AppQueryScalarKind::Timestamp => value
            .as_str()
            .is_some_and(|value| DateTime::parse_from_rfc3339(value).is_ok()),
    };
    if !valid {
        return Err(AppQuerySemanticsError::ValueTypeMismatch {
            kind: contract.kind,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppCursorVersion {
    V1,
}

/// Server-stored opaque cursor evidence. Clients carry only `cursor_ref`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppQueryCursorEvidence {
    version: AppCursorVersion,
    cursor_ref: AppReference,
    source_installation_id: AppInstallationId,
    schema_revision: AppRevision,
    dataset_generation: u64,
    query_digest: AppDigest,
    stable_order_digest: AppDigest,
    last_record_id: AppRecordId,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl AppQueryCursorEvidence {
    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        request: &AppQueryRequest,
        schema: &AppTypedQuerySchema,
        schema_revision: AppRevision,
        dataset_generation: u64,
        last_record_id: AppRecordId,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppQuerySemanticsError> {
        validate_typed_query(request, schema)?;
        if dataset_generation == 0 {
            return Err(AppQuerySemanticsError::InvalidDatasetGeneration);
        }
        if expires_at <= issued_at {
            return Err(AppQuerySemanticsError::InvalidCursorWindow);
        }
        let query_digest = query_identity_digest(request)?;
        let stable_order_digest = stable_order_digest(request)?;
        #[derive(Serialize)]
        struct CursorIdentity<'a> {
            version: AppCursorVersion,
            source_installation_id: &'a AppInstallationId,
            schema_revision: AppRevision,
            dataset_generation: u64,
            query_digest: &'a AppDigest,
            stable_order_digest: &'a AppDigest,
            last_record_id: &'a AppRecordId,
            issued_at: &'a DateTime<Utc>,
            expires_at: &'a DateTime<Utc>,
        }
        let identity = CursorIdentity {
            version: AppCursorVersion::V1,
            source_installation_id: &request.source_installation_id,
            schema_revision,
            dataset_generation,
            query_digest: &query_digest,
            stable_order_digest: &stable_order_digest,
            last_record_id: &last_record_id,
            issued_at: &issued_at,
            expires_at: &expires_at,
        };
        let bytes = canonical_json_bytes(&serde_json::to_value(identity)?)?;
        let opaque = blake3::hash(&bytes).to_hex();
        Ok(Self {
            version: AppCursorVersion::V1,
            cursor_ref: AppReference::parse(format!("cursor:v1:{opaque}"))?,
            source_installation_id: request.source_installation_id.clone(),
            schema_revision,
            dataset_generation,
            query_digest,
            stable_order_digest,
            last_record_id,
            issued_at,
            expires_at,
        })
    }

    pub fn cursor_ref(&self) -> &AppReference {
        &self.cursor_ref
    }

    pub fn dataset_generation(&self) -> u64 {
        self.dataset_generation
    }

    pub fn encode_trusted_store(&self) -> Result<Vec<u8>, AppQuerySemanticsError> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > 16_384 {
            return Err(AppQuerySemanticsError::CursorStorageCorrupt);
        }
        Ok(bytes)
    }

    pub fn decode_and_validate_trusted_store(
        bytes: &[u8],
        request: &AppQueryRequest,
        schema: &AppTypedQuerySchema,
        current_schema_revision: AppRevision,
        available_snapshot_generation: u64,
        now: &DateTime<Utc>,
    ) -> Result<AppValidatedQueryCursor, AppQuerySemanticsError> {
        if bytes.len() > 16_384 {
            return Err(AppQuerySemanticsError::CursorStorageCorrupt);
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct StoredCursor {
            version: AppCursorVersion,
            cursor_ref: AppReference,
            source_installation_id: AppInstallationId,
            schema_revision: AppRevision,
            dataset_generation: u64,
            query_digest: AppDigest,
            stable_order_digest: AppDigest,
            last_record_id: AppRecordId,
            issued_at: DateTime<Utc>,
            expires_at: DateTime<Utc>,
        }
        let value = decode_bounded_json_value(bytes, &AppContractLimits::default())
            .map_err(|_| AppQuerySemanticsError::CursorStorageCorrupt)?;
        let stored: StoredCursor = serde_json::from_value(value)
            .map_err(|_| AppQuerySemanticsError::CursorStorageCorrupt)?;
        let decoded = Self {
            version: stored.version,
            cursor_ref: stored.cursor_ref,
            source_installation_id: stored.source_installation_id,
            schema_revision: stored.schema_revision,
            dataset_generation: stored.dataset_generation,
            query_digest: stored.query_digest,
            stable_order_digest: stored.stable_order_digest,
            last_record_id: stored.last_record_id,
            issued_at: stored.issued_at,
            expires_at: stored.expires_at,
        };
        // A client changing its query is an invalid continuation, not corrupt
        // server storage. Keep reminting below to detect tampered stored evidence.
        if query_identity_digest(request)? != decoded.query_digest
            || stable_order_digest(request)? != decoded.stable_order_digest
        {
            return Err(AppQuerySemanticsError::CursorIdentityMismatch);
        }
        let reminted = Self::mint(
            request,
            schema,
            decoded.schema_revision,
            decoded.dataset_generation,
            decoded.last_record_id.clone(),
            decoded.issued_at,
            decoded.expires_at,
        )?;
        if reminted != decoded {
            return Err(AppQuerySemanticsError::CursorStorageCorrupt);
        }
        decoded.validate_reuse(
            request,
            schema,
            current_schema_revision,
            available_snapshot_generation,
            now,
        )
    }

    pub fn validate_reuse(
        &self,
        request: &AppQueryRequest,
        schema: &AppTypedQuerySchema,
        current_schema_revision: AppRevision,
        available_snapshot_generation: u64,
        now: &DateTime<Utc>,
    ) -> Result<AppValidatedQueryCursor, AppQuerySemanticsError> {
        validate_typed_query(request, schema)?;
        if now < &self.issued_at || now >= &self.expires_at {
            return Err(AppQuerySemanticsError::CursorExpired);
        }
        if request.cursor.as_ref() != Some(&self.cursor_ref)
            || request.source_installation_id != self.source_installation_id
        {
            return Err(AppQuerySemanticsError::CursorIdentityMismatch);
        }
        if current_schema_revision != self.schema_revision
            || available_snapshot_generation != self.dataset_generation
            || query_identity_digest(request)? != self.query_digest
            || stable_order_digest(request)? != self.stable_order_digest
        {
            return Err(AppQuerySemanticsError::CursorSnapshotStale);
        }
        Ok(AppValidatedQueryCursor {
            cursor_ref: self.cursor_ref.clone(),
            schema_revision: self.schema_revision,
            dataset_generation: self.dataset_generation,
            last_record_id: self.last_record_id.clone(),
            issued_at: self.issued_at,
            expires_at: self.expires_at,
        })
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppValidatedQueryCursor {
    cursor_ref: AppReference,
    schema_revision: AppRevision,
    dataset_generation: u64,
    last_record_id: AppRecordId,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl AppValidatedQueryCursor {
    pub fn last_record_id(&self) -> &AppRecordId {
        &self.last_record_id
    }

    pub fn dataset_generation(&self) -> u64 {
        self.dataset_generation
    }

    pub fn issued_at(&self) -> DateTime<Utc> {
        self.issued_at
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

fn query_identity_digest(request: &AppQueryRequest) -> Result<AppDigest, AppQuerySemanticsError> {
    let mut identity = request.clone();
    identity.cursor = None;
    let bytes = canonical_json_bytes(&serde_json::to_value(identity)?)?;
    Ok(AppDigest::blake3(&bytes))
}

fn stable_order_digest(request: &AppQueryRequest) -> Result<AppDigest, AppQuerySemanticsError> {
    #[derive(Serialize)]
    struct StableOrder<'a> {
        requested: &'a [super::models::AppQueryOrder],
        implicit_record_id_tiebreaker: bool,
        text_normalization: AppTextComparisonNormalization,
        missing_values: AppMissingValueSemantics,
    }
    let bytes = canonical_json_bytes(&serde_json::to_value(StableOrder {
        requested: &request.order,
        implicit_record_id_tiebreaker: true,
        text_normalization: AppTextComparisonNormalization::UnicodeNfkcCaseSensitive,
        missing_values: AppMissingValueSemantics::DistinctFromNullAndExcluded,
    })?)?;
    Ok(AppDigest::blake3(&bytes))
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppQuerySemanticsError {
    #[error(transparent)]
    Contract(#[from] super::models::AppContractError),
    #[error("query field contract exceeds the {limit} field ceiling")]
    FieldContractLimit { limit: usize },
    #[error("query field contract contains a duplicate path")]
    DuplicateFieldContract,
    #[error("query enum field contract must contain between 1 and {limit} canonical values")]
    InvalidEnumFieldContract { limit: usize },
    #[error("query relation contract is invalid")]
    InvalidRelationContract,
    #[error("query relation contract contains a duplicate relation")]
    DuplicateRelationContract,
    #[error("query references unknown field `{0}`")]
    UnknownField(String),
    #[error("query ordering field `{0}` is not indexed and sortable")]
    UnstableOrderField(String),
    #[error("query operator {operator:?} is invalid for {kind:?}")]
    OperatorTypeMismatch {
        kind: AppQueryScalarKind,
        operator: AppComparisonOperator,
    },
    #[error("query value does not match {kind:?}")]
    ValueTypeMismatch { kind: AppQueryScalarKind },
    #[error("null comparisons must use the explicit is_null predicate")]
    NullRequiresIsNull,
    #[error("in predicate must contain at least one value")]
    EmptyInPredicate,
    #[error("is_null cannot target non-nullable field `{0}`")]
    NonNullableNullPredicate(String),
    #[error("query expands unknown relation `{0}`")]
    UnknownRelation(String),
    #[error("query expands relation `{0}` more than once")]
    DuplicateRelationExpansion(String),
    #[error("query relation `{relation}` exceeds its compiled depth ceiling {limit}")]
    RelationDepthExceeded { relation: String, limit: u16 },
    #[error("query relation `{relation}` selects unknown field `{field}`")]
    UnknownRelationField { relation: String, field: String },
    #[error("cursor dataset generation must be positive")]
    InvalidDatasetGeneration,
    #[error("cursor expiry must be later than issue time")]
    InvalidCursorWindow,
    #[error("cursor is expired or not yet live")]
    CursorExpired,
    #[error("cursor does not belong to this request or installation")]
    CursorIdentityMismatch,
    #[error("cursor schema, query, ordering or retained snapshot is stale")]
    CursorSnapshotStale,
    #[error("server-held cursor evidence is corrupt")]
    CursorStorageCorrupt,
    #[error("failed to encode query identity: {0}")]
    Encoding(String),
}

impl From<serde_json::Error> for AppQuerySemanticsError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encoding(error.to_string())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::magician_v2::apps::models::{
        AppContractError, AppName, AppOrderDirection, AppPredicate, AppProtocolVersion,
        AppQueryOrder,
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 15, 0, 0, second)
            .single()
            .unwrap()
    }

    fn field(value: &str) -> AppFieldPath {
        AppFieldPath::parse(value).unwrap()
    }

    fn schema() -> AppTypedQuerySchema {
        AppTypedQuerySchema::from_compiled_fields(vec![
            AppQueryFieldContract::from_compiled_schema(
                field("title"),
                AppQueryScalarKind::Text,
                false,
                true,
                true,
            ),
            AppQueryFieldContract::from_compiled_schema(
                field("duration"),
                AppQueryScalarKind::Integer,
                true,
                true,
                true,
            ),
        ])
        .unwrap()
    }

    fn schema_with_relation() -> AppTypedQuerySchema {
        AppTypedQuerySchema::from_compiled_contracts(
            vec![AppQueryFieldContract::from_compiled_schema(
                field("title"),
                AppQueryScalarKind::Text,
                false,
                true,
                true,
            )],
            vec![AppQueryRelationContract::from_compiled_schema(
                AppName::parse("author").unwrap(),
                BTreeSet::from([field("display_name")]),
                2,
            )
            .unwrap()],
        )
        .unwrap()
    }

    fn request() -> AppQueryRequest {
        AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("video").unwrap(),
            select: vec![field("title")],
            predicate: Some(AppPredicate {
                root: 0,
                nodes: vec![AppPredicateNode::Compare {
                    field: field("duration"),
                    operator: AppComparisonOperator::GreaterThan,
                    value: json!(60),
                }],
            }),
            order: vec![AppQueryOrder {
                field: field("duration"),
                direction: AppOrderDirection::Descending,
            }],
            cursor: None,
            limit: 100,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("library").unwrap(),
        }
    }

    #[test]
    fn typed_predicates_reject_operator_value_and_null_dialects() {
        let mut invalid_operator = request();
        let AppPredicateNode::Compare { operator, .. } =
            &mut invalid_operator.predicate.as_mut().unwrap().nodes[0]
        else {
            unreachable!()
        };
        *operator = AppComparisonOperator::Contains;
        assert!(matches!(
            validate_typed_query(&invalid_operator, &schema()),
            Err(AppQuerySemanticsError::OperatorTypeMismatch { .. })
        ));

        let mut null = request();
        let AppPredicateNode::Compare { value, .. } =
            &mut null.predicate.as_mut().unwrap().nodes[0]
        else {
            unreachable!()
        };
        *value = Value::Null;
        assert!(matches!(
            validate_typed_query(&null, &schema()),
            Err(AppQuerySemanticsError::Contract(
                AppContractError::InvalidField {
                    field: "predicate.value",
                    ..
                }
            ))
        ));
    }

    #[test]
    fn cursor_binds_query_order_schema_and_snapshot_generation() {
        let base = request();
        let evidence = AppQueryCursorEvidence::mint(
            &base,
            &schema(),
            AppRevision::new(3).unwrap(),
            9,
            AppRecordId::parse("record_42").unwrap(),
            time(1),
            time(10),
        )
        .unwrap();
        let mut next = base.clone();
        next.cursor = Some(evidence.cursor_ref().clone());
        evidence
            .validate_reuse(&next, &schema(), AppRevision::new(3).unwrap(), 9, &time(2))
            .unwrap();
        next.limit = 99;
        assert!(matches!(
            evidence.validate_reuse(&next, &schema(), AppRevision::new(3).unwrap(), 9, &time(2),),
            Err(AppQuerySemanticsError::CursorSnapshotStale)
        ));
        let different_expiry = AppQueryCursorEvidence::mint(
            &base,
            &schema(),
            AppRevision::new(3).unwrap(),
            9,
            AppRecordId::parse("record_42").unwrap(),
            time(1),
            time(11),
        )
        .unwrap();
        assert_ne!(evidence.cursor_ref(), different_expiry.cursor_ref());
    }

    #[test]
    fn relation_expansion_is_bounded_by_the_compiled_relation_contract() {
        let mut request = request();
        request.select = vec![field("title")];
        request.predicate = None;
        request.order.clear();
        request.relation_expansions = vec![super::super::models::AppRelationExpansion {
            relation: AppName::parse("author").unwrap(),
            select: vec![field("display_name")],
            max_depth: 2,
            max_rows: 10,
        }];
        validate_typed_query(&request, &schema_with_relation()).unwrap();

        request.relation_expansions[0].max_depth = 3;
        assert!(matches!(
            validate_typed_query(&request, &schema_with_relation()),
            Err(AppQuerySemanticsError::RelationDepthExceeded { .. })
        ));
        request.relation_expansions[0].max_depth = 1;
        request.relation_expansions[0].select = vec![field("private_field")];
        assert!(matches!(
            validate_typed_query(&request, &schema_with_relation()),
            Err(AppQuerySemanticsError::UnknownRelationField { .. })
        ));
    }

    #[test]
    fn enum_and_reference_predicates_require_compiled_canonical_values() {
        let typed = AppTypedQuerySchema::from_compiled_contracts(
            vec![
                AppQueryFieldContract::from_compiled_enum_schema(
                    field("status"),
                    false,
                    true,
                    true,
                    BTreeSet::from([
                        AppName::parse("queued").unwrap(),
                        AppName::parse("ready").unwrap(),
                    ]),
                )
                .unwrap(),
                AppQueryFieldContract::from_compiled_schema(
                    field("owner"),
                    AppQueryScalarKind::Reference,
                    false,
                    true,
                    true,
                ),
            ],
            Vec::new(),
        )
        .unwrap();
        let mut request = request();
        request.select = vec![field("status")];
        request.order.clear();
        request.predicate = Some(AppPredicate {
            root: 0,
            nodes: vec![AppPredicateNode::Compare {
                field: field("status"),
                operator: AppComparisonOperator::Equal,
                value: json!("invented"),
            }],
        });
        assert!(matches!(
            validate_typed_query(&request, &typed),
            Err(AppQuerySemanticsError::ValueTypeMismatch {
                kind: AppQueryScalarKind::Enum
            })
        ));
        request.predicate = Some(AppPredicate {
            root: 0,
            nodes: vec![AppPredicateNode::Compare {
                field: field("owner"),
                operator: AppComparisonOperator::Equal,
                value: json!("not a canonical record id"),
            }],
        });
        assert!(matches!(
            validate_typed_query(&request, &typed),
            Err(AppQuerySemanticsError::ValueTypeMismatch {
                kind: AppQueryScalarKind::Reference
            })
        ));
    }

    #[test]
    fn cursor_and_compiled_schema_evidence_are_not_transport_constructible() {
        static_assertions::assert_not_impl_any!(
            AppTypedQuerySchema: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppQueryCursorEvidence: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppValidatedQueryCursor: serde::de::DeserializeOwned
        );
    }

    #[test]
    fn keyset_cursor_rejects_changed_query_and_mode_as_client_errors() {
        let schema = schema();
        let mut request = request();
        request.pagination = super::super::models::AppQueryPagination::Keyset;
        let evidence = AppQueryCursorEvidence::mint(
            &request,
            &schema,
            AppRevision::new(3).unwrap(),
            7,
            AppRecordId::parse("record_1").unwrap(),
            time(1),
            time(3),
        )
        .unwrap();
        request.cursor = Some(evidence.cursor_ref().clone());
        let encoded = evidence.encode_trusted_store().unwrap();
        let mut changed = request.clone();
        changed.limit += 1;
        for changed in [
            changed,
            AppQueryRequest {
                pagination: super::super::models::AppQueryPagination::Snapshot,
                ..request
            },
        ] {
            assert!(matches!(
                AppQueryCursorEvidence::decode_and_validate_trusted_store(
                    &encoded,
                    &changed,
                    &schema,
                    AppRevision::new(3).unwrap(),
                    7,
                    &time(2)
                ),
                Err(AppQuerySemanticsError::CursorIdentityMismatch)
            ));
        }
    }

    #[test]
    fn persisted_cursor_is_reminted_and_tamper_fails_closed() {
        let schema = schema();
        let mut request = request();
        let evidence = AppQueryCursorEvidence::mint(
            &request,
            &schema,
            AppRevision::new(3).unwrap(),
            7,
            AppRecordId::parse("record_1").unwrap(),
            time(1),
            time(3),
        )
        .unwrap();
        request.cursor = Some(evidence.cursor_ref().clone());
        let encoded = evidence.encode_trusted_store().unwrap();
        AppQueryCursorEvidence::decode_and_validate_trusted_store(
            &encoded,
            &request,
            &schema,
            AppRevision::new(3).unwrap(),
            7,
            &time(2),
        )
        .unwrap();

        let mut tampered: Value = serde_json::from_slice(&encoded).unwrap();
        tampered["dataset_generation"] = json!(8);
        assert!(matches!(
            AppQueryCursorEvidence::decode_and_validate_trusted_store(
                &serde_json::to_vec(&tampered).unwrap(),
                &request,
                &schema,
                AppRevision::new(3).unwrap(),
                7,
                &time(2),
            ),
            Err(AppQuerySemanticsError::CursorStorageCorrupt)
        ));
    }
}
