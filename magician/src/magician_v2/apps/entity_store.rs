//! Lazy authenticated owner for the Phase-2 generic app entity store.
//!
//! The service delegates every connection/path/admission decision to
//! [`AppRegistryService`]. Phase 2A resolves the exact active schema, Phase 2B
//! owns deterministic reads, Phase 2C owns transactional optimistic mutations,
//! and Phase 2E exposes only the authenticated owner/direct-personal-agent
//! adapters over this same service.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use rusqlite::{params, types::Value as SqlValue, Connection, OptionalExtension};
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use super::{
    authority::AuthenticatedAppScope,
    boundary::{AppBoundaryError, AppStoreAuthorityFence, AppStoreReadAudience},
    composition::AppCompositionTransferReceipt,
    entity_index::{project_scalar_index, AppEntityIndexError},
    lifecycle::AppInstallationStatus,
    manifest::AppManifestBehaviorInputSelector,
    models::{
        decode_bounded_json_value, AppComparisonOperator, AppContractError, AppContractLimits,
        AppDataEnvelope, AppDataSource, AppDigest, AppFieldPath, AppHandlingLabels,
        AppInstallationId, AppName, AppOrderDirection, AppPredicate, AppPredicateNode,
        AppProtocolVersion, AppQueryPage, AppQueryRequest, AppRecordId, AppRecordProjection,
        AppReference, AppRelationExpansion, AppRevision, AppScopeBindingRef, AppSourceRecordFence,
        AppSourceRef, AppSourceRefKind, ValidateAppContract,
    },
    query_semantics::{
        validate_typed_query, AppQueryCursorEvidence, AppQueryScalarKind, AppQuerySemanticsError,
    },
    records::{
        validate_policy, AppDataHandlingPolicy, AppGrantRevision, AppInstallation,
        AppPackageRevision, AppPersonalAgentAccess, AppSchemaRevision, AppScope,
    },
    registry::{AppRegistryError, AppRegistryService},
    schema_compiler::{
        runtime_contracts_from_revision, source_entity_schema_digest_from_revision,
        AppEntityRuntimeContract, AppSchemaCompilerError,
    },
};

/// Rows one unindexed snapshot read will return before it refuses.
///
/// `pub(crate)` because a caller that must PROVE it read everything — the
/// Town Square corpus migration is the first — has to know the bound it is
/// proving against, rather than discovering it as a failure after writing.
pub(crate) const MAX_QUERY_SNAPSHOT_ROWS: usize = 10_000;
const MAX_QUERY_SNAPSHOT_BYTES: usize = 4 * 1_024 * 1_024;
/// Decoded payload bytes one unindexed snapshot read will scan before it
/// refuses. `pub(crate)` for the same reason as `MAX_QUERY_SNAPSHOT_ROWS`:
/// a caller that must prove it read everything has to know both bounds.
pub(crate) const MAX_QUERY_SCAN_BYTES: usize = 64 * 1_024 * 1_024;
const MAX_QUERY_CURSORS_PER_INSTALLATION: i64 = 256;
const MAX_QUERY_CURSOR_BYTES_PER_INSTALLATION: i64 = 64 * 1_024 * 1_024;
const QUERY_SNAPSHOT_MAGIC: &[u8; 5] = b"ASNP\x01";
const QUERY_CURSOR_TTL: Duration = Duration::from_secs(10 * 60);
// Keyset cursors retain one boundary, not a revision snapshot. Let a reader
// return after a normal day without losing their opened history.
const KEYSET_CURSOR_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Exact active entity-schema snapshot resolved under one authenticated scope.
/// Fields are private and the type is not deserializable, so transport input
/// cannot mint store authority or executable query contracts.
#[derive(Debug, Clone)]
pub struct ActiveAppEntitySchema {
    installation_id: AppInstallationId,
    installation_generation: u64,
    package_revision_ref: AppReference,
    active_surface_revision: Option<AppRevision>,
    grant: AppGrantRevision,
    schema: AppSchemaRevision,
    runtime_contracts: Arc<BTreeMap<AppName, AppEntityRuntimeContract>>,
}

/// One destination execution snapshot, optionally fenced with its complete
/// brokered source, resolved in a single SQLite read transaction.
///
/// The type has no wire representation. Consumers may use it only to finish a
/// pure authority intersection without another asynchronous registry read.
pub struct AppRuntimeAuthoritySnapshot {
    installation: AppInstallation,
    active: ActiveAppEntitySchema,
    source_record_digest: Option<AppDigest>,
    source_record_payload: Option<Value>,
}

impl AppRuntimeAuthoritySnapshot {
    pub fn into_parts(self) -> (AppInstallation, ActiveAppEntitySchema) {
        (self.installation, self.active)
    }

    /// Consume a source-fenced snapshot without cloning the bounded payload.
    pub fn into_source_parts(
        self,
    ) -> (
        AppInstallation,
        ActiveAppEntitySchema,
        Option<AppDigest>,
        Option<Value>,
    ) {
        (
            self.installation,
            self.active,
            self.source_record_digest,
            self.source_record_payload,
        )
    }

    /// Canonical payload digest of the exact current source head when this
    /// snapshot was resolved through `runtime_contribution_source_snapshot`.
    /// The digest is recomputed while decoding the record in the same SQLite
    /// transaction as the installation/grant/schema authority checks.
    pub fn source_record_digest(&self) -> Option<&AppDigest> {
        self.source_record_digest.as_ref()
    }

    /// Already-decoded, schema-validated payload of the exact source head.
    /// This remains inside the trusted process and is never a wire projection;
    /// destination seams may use it to prove that signed command semantics
    /// match the record whose digest they revalidated.
    pub fn source_record_payload(&self) -> Option<&Value> {
        self.source_record_payload.as_ref()
    }
}

/// Server-owned proof that one personal-agent record projection still matches
/// the current store head and the current installation/grant/schema tuple.
/// This type is intentionally not deserializable: a copied query page is only
/// input to revalidation, never authority by itself.
#[derive(Debug, Clone)]
pub struct AppRevalidatedRecordProjection {
    active: ActiveAppEntitySchema,
    projection: AppRecordProjection,
    source_ref: AppSourceRef,
    handling_policy: AppDataHandlingPolicy,
    read_audience: AppStoreReadAudience,
}

impl AppRevalidatedRecordProjection {
    pub fn active(&self) -> &ActiveAppEntitySchema {
        &self.active
    }

    pub fn projection(&self) -> &AppRecordProjection {
        &self.projection
    }

    pub fn source_ref(&self) -> &AppSourceRef {
        &self.source_ref
    }

    pub fn handling_policy(&self) -> &AppDataHandlingPolicy {
        &self.handling_policy
    }

    pub fn read_audience(&self) -> &AppStoreReadAudience {
        &self.read_audience
    }

    pub(crate) fn memory_source(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<super::memory::ResolvedAppMemorySource, super::memory::AppMemoryEligibilityError>
    {
        super::memory::ResolvedAppMemorySource::from_revalidated_projection(
            authenticated_scope,
            self.active.installation_id().clone(),
            self.active.installation_generation(),
            self.active.package_revision_ref().clone(),
            self.active.grant_revision(),
            self.active.schema_revision(),
            &self.projection,
            self.source_ref.clone(),
            self.handling_policy.clone(),
            now,
        )
    }
}

/// One query page paired with the complete effective policy resolved in the
/// same SQLite snapshot. The policy deliberately remains outside the wire
/// page: trusted adapters carry it beside the bytes instead of treating a
/// caller-visible digest as reconstructable authority.
pub struct AppGovernedQueryPage {
    page: AppQueryPage,
    handling_policy: AppDataHandlingPolicy,
}

/// Entity-store-owned, sealed locator for one exact Recipe Query projection.
///
/// The raw record id exists only in this private lifecycle document. Recipe
/// values carry `logical_ref`, which remains an opaque rejection token. A Get
/// can disclose even that opaque projection only after replaying the original
/// bounded query through current workflow/store authority and asking this
/// locator to match the exact scope, installation, schema, entity, revision,
/// selected values, policies and provenance again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppRecipeRecordLocator {
    schema: String,
    logical_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    grant_revision: AppRevision,
    entity: AppName,
    record_id: AppRecordId,
    record_revision: AppRevision,
    value_schema_ref: AppReference,
    projection_digest: AppDigest,
    query_parameters: BTreeMap<String, Value>,
    handling_labels: AppHandlingLabels,
    source_refs_digest: AppDigest,
    locator_digest: AppDigest,
}

#[derive(Serialize)]
struct AppRecipeRecordLocatorIdentity<'a> {
    schema: &'a str,
    logical_ref: &'a AppReference,
    scope_binding_ref: &'a AppScopeBindingRef,
    installation_id: &'a AppInstallationId,
    package_revision_ref: &'a AppReference,
    schema_revision: AppRevision,
    grant_revision: AppRevision,
    entity: &'a AppName,
    record_id: &'a AppRecordId,
    record_revision: AppRevision,
    value_schema_ref: &'a AppReference,
    projection_digest: &'a AppDigest,
    query_parameters: &'a BTreeMap<String, Value>,
    handling_labels: &'a AppHandlingLabels,
    source_refs_digest: &'a AppDigest,
}

impl AppRecipeRecordLocator {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn seal(
        logical_ref: AppReference,
        value_schema_ref: AppReference,
        query_parameters: BTreeMap<String, Value>,
        page: &AppQueryPage,
        projection: &AppRecordProjection,
        projection_digest: AppDigest,
    ) -> Result<Self, AppEntityStoreError> {
        if !logical_ref.as_str().starts_with("entity:")
            || page.envelope.value.len() != 1
            || page.next_cursor.is_some()
            || page.envelope.value.first() != Some(projection)
            || projection.entity.as_str()
                != query_parameters
                    .get("entity")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            || query_parameters.get("limit").and_then(Value::as_u64) != Some(1)
            || query_parameters
                .get("cursor")
                .is_some_and(|value| !value.is_null())
            || page.envelope.content_digest
                != AppDigest::blake3_canonical_json(&serde_json::to_value(&page.envelope.value)?)?
            || projection_digest
                != AppDigest::blake3_canonical_json(&serde_json::to_value(projection)?)?
        {
            return Err(AppEntityStoreError::RecipeRecordLocatorSubstitution);
        }
        let source_refs_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&page.envelope.source_refs)?)?;
        let mut locator = Self {
            schema: "magician.app-recipe-record-locator.v1".to_owned(),
            logical_ref,
            scope_binding_ref: page.envelope.scope_binding_ref.clone(),
            installation_id: page.envelope.installation_id.clone(),
            package_revision_ref: page.envelope.package_revision_ref.clone(),
            schema_revision: page.envelope.schema_revision,
            grant_revision: page.envelope.grant_revision,
            entity: projection.entity.clone(),
            record_id: projection.record_id.clone(),
            record_revision: projection.record_revision,
            value_schema_ref,
            projection_digest,
            query_parameters,
            handling_labels: page.envelope.handling_labels.clone(),
            source_refs_digest,
            locator_digest: AppDigest::blake3(b"pending-app-recipe-record-locator"),
        };
        locator.locator_digest = locator.expected_digest()?;
        Ok(locator)
    }

    fn identity(&self) -> AppRecipeRecordLocatorIdentity<'_> {
        AppRecipeRecordLocatorIdentity {
            schema: &self.schema,
            logical_ref: &self.logical_ref,
            scope_binding_ref: &self.scope_binding_ref,
            installation_id: &self.installation_id,
            package_revision_ref: &self.package_revision_ref,
            schema_revision: self.schema_revision,
            grant_revision: self.grant_revision,
            entity: &self.entity,
            record_id: &self.record_id,
            record_revision: self.record_revision,
            value_schema_ref: &self.value_schema_ref,
            projection_digest: &self.projection_digest,
            query_parameters: &self.query_parameters,
            handling_labels: &self.handling_labels,
            source_refs_digest: &self.source_refs_digest,
        }
    }

    fn expected_digest(&self) -> Result<AppDigest, AppEntityStoreError> {
        AppDigest::blake3_canonical_json(&serde_json::to_value(self.identity())?)
            .map_err(Into::into)
    }

    pub(crate) fn validate_integrity(&self) -> Result<(), AppEntityStoreError> {
        if self.schema != "magician.app-recipe-record-locator.v1"
            || self.locator_digest != self.expected_digest()?
            || !self.logical_ref.as_str().starts_with("entity:")
            || self.query_parameters.get("limit").and_then(Value::as_u64) != Some(1)
            || self.query_parameters.get("entity").and_then(Value::as_str)
                != Some(self.entity.as_str())
        {
            return Err(AppEntityStoreError::RecipeRecordLocatorSubstitution);
        }
        Ok(())
    }

    pub(crate) fn verify_replayed_page(
        &self,
        page: &AppQueryPage,
    ) -> Result<(), AppEntityStoreError> {
        self.validate_integrity()?;
        let projection = page
            .envelope
            .value
            .first()
            .filter(|_| page.envelope.value.len() == 1 && page.next_cursor.is_none())
            .ok_or(AppEntityStoreError::StaleRecordProjection)?;
        let projection_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(projection)?)?;
        let source_refs_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&page.envelope.source_refs)?)?;
        if page.envelope.scope_binding_ref != self.scope_binding_ref
            || page.envelope.installation_id != self.installation_id
            || page.envelope.package_revision_ref != self.package_revision_ref
            || page.envelope.schema_revision != self.schema_revision
            || page.envelope.grant_revision != self.grant_revision
            || projection.entity != self.entity
            || projection.record_id != self.record_id
            || projection.record_revision != self.record_revision
            || projection_digest != self.projection_digest
            || page.envelope.handling_labels != self.handling_labels
            || source_refs_digest != self.source_refs_digest
        {
            return Err(AppEntityStoreError::StaleRecordProjection);
        }
        Ok(())
    }

    pub(crate) fn logical_ref(&self) -> &AppReference {
        &self.logical_ref
    }

    pub(crate) fn entity(&self) -> &AppName {
        &self.entity
    }

    pub(crate) fn record_revision(&self) -> AppRevision {
        self.record_revision
    }

    pub(crate) fn value_schema_ref(&self) -> &AppReference {
        &self.value_schema_ref
    }

    pub(crate) fn projection_digest(&self) -> &AppDigest {
        &self.projection_digest
    }

    pub(crate) fn query_parameters(&self) -> &BTreeMap<String, Value> {
        &self.query_parameters
    }

    pub(crate) fn handling_labels(&self) -> &AppHandlingLabels {
        &self.handling_labels
    }
}

impl AppGovernedQueryPage {
    pub fn page(&self) -> &AppQueryPage {
        &self.page
    }

    pub fn handling_policy(&self) -> &AppDataHandlingPolicy {
        &self.handling_policy
    }

    pub fn into_parts(self) -> (AppQueryPage, AppDataHandlingPolicy) {
        (self.page, self.handling_policy)
    }
}

impl ActiveAppEntitySchema {
    pub fn from_reviewed_update(
        installation_generation: u64,
        active_surface_revision: AppRevision,
        grant: AppGrantRevision,
        schema: AppSchemaRevision,
    ) -> Result<Self, AppSchemaCompilerError> {
        let runtime_contracts = runtime_contracts_from_revision(&schema)?;
        Ok(Self {
            installation_id: schema.installation_id.clone(),
            installation_generation,
            package_revision_ref: schema.package_revision_ref.clone(),
            active_surface_revision: Some(active_surface_revision),
            grant,
            schema,
            runtime_contracts,
        })
    }

    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub fn installation_generation(&self) -> u64 {
        self.installation_generation
    }

    pub fn package_revision_ref(&self) -> &AppReference {
        &self.package_revision_ref
    }

    pub fn active_surface_revision(&self) -> Option<AppRevision> {
        self.active_surface_revision
    }

    pub fn schema_revision(&self) -> AppRevision {
        self.schema.revision
    }

    pub fn grant_revision(&self) -> AppRevision {
        self.grant.revision
    }

    pub fn grant(&self) -> &AppGrantRevision {
        &self.grant
    }

    pub fn schema(&self) -> &AppSchemaRevision {
        &self.schema
    }

    pub fn runtime_contract(&self, entity: &AppName) -> Option<&AppEntityRuntimeContract> {
        self.runtime_contracts.get(entity)
    }

    pub fn runtime_contracts(&self) -> impl Iterator<Item = (&AppName, &AppEntityRuntimeContract)> {
        self.runtime_contracts.iter()
    }

    /// Select bounded current-head reads before a caller binds request bytes to
    /// its resource permit. This is query planning, never read authority.
    pub(crate) fn supports_keyset_query(&self, request: &AppQueryRequest) -> bool {
        let Some(runtime) = self.runtime_contract(&request.entity) else {
            return false;
        };
        self.validate_query_contract(request).is_ok()
            && request.order.len() <= 1
            && compile_indexed_query_filter(request, runtime).is_some()
    }

    /// Pure request validation against this server-resolved schema snapshot.
    /// This reads no records and creates no cursor. It is useful before
    /// resource dispatch, but never replaces the current transaction's
    /// authority, schema, cursor or record-policy checks.
    pub fn validate_query_contract(
        &self,
        request: &AppQueryRequest,
    ) -> Result<(), AppEntityStoreError> {
        request.validate_app_contract(&AppContractLimits::default())?;
        if request.source_installation_id != self.installation_id {
            return Err(AppEntityStoreError::ScopeOrIdentityMismatch);
        }
        let runtime = self
            .runtime_contract(&request.entity)
            .ok_or_else(|| AppEntityStoreError::UnknownEntity(request.entity.to_string()))?;
        validate_typed_query(request, runtime.query_schema())?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AppEntityStoreService {
    pub registry: AppRegistryService,
}

impl AppEntityStoreService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self { registry }
    }

    /// Resolve the exact enabled installation/schema/package tuple in one
    /// read-only connection. Missing scoped storage remains lazy; any corrupt,
    /// stale, disabled or mismatched durable evidence fails closed.
    pub async fn active_schema(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Option<ActiveAppEntitySchema>, AppEntityStoreError> {
        let installation_id = installation_id.clone();
        let resolved = self
            .registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(resolve_active_schema(connection, scope, &installation_id))
            })
            .await?;
        match resolved {
            None => Ok(None),
            Some(result) => result,
        }
    }

    /// Resolve current destination execution authority and, when present, the
    /// complete brokered-source record/policy fence at one SQLite snapshot.
    /// Loading immutable package material belongs before this call; consumers
    /// must perform no further asynchronous work before using the snapshot.
    pub(crate) async fn runtime_authority_snapshot(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        destination_installation_id: &AppInstallationId,
        source_receipt: Option<&AppCompositionTransferReceipt>,
        now: DateTime<Utc>,
    ) -> Result<AppRuntimeAuthoritySnapshot, AppEntityStoreError> {
        let destination_installation_id = destination_installation_id.clone();
        let source_receipt = source_receipt.cloned();
        self.registry
            .execute_scoped_typed_read(authenticated_scope, &now, move |connection, scope| {
                let transaction = connection.unchecked_transaction()?;
                if let Some(receipt) = source_receipt.as_ref() {
                    revalidate_composition_source_in_snapshot(&transaction, scope, receipt)?;
                }
                let installation = load_installation_in_snapshot(
                    &transaction,
                    scope,
                    &destination_installation_id,
                )?
                .ok_or(AppEntityStoreError::MissingInstallation)?;
                let active =
                    resolve_active_schema(&transaction, scope, &destination_installation_id)?
                        .ok_or(AppEntityStoreError::MissingInstallation)?;
                if installation.lifecycle.generation != active.installation_generation()
                    || installation.package_revision_ref != *active.package_revision_ref()
                    || installation.grant_revision != Some(active.grant_revision())
                    || installation.active_schema_revision != Some(active.schema_revision())
                    || installation.active_surface_revision != active.active_surface_revision()
                {
                    return Err(AppEntityStoreError::StaleSchemaBinding);
                }
                transaction.commit()?;
                Ok(AppRuntimeAuthoritySnapshot {
                    installation,
                    active,
                    source_record_digest: None,
                    source_record_payload: None,
                })
            })
            .await?
            .ok_or(AppEntityStoreError::MissingScopedStore)
    }

    /// Reopen the current installation/grant/schema tuple and one exact live
    /// contribution source head in the same SQLite snapshot. A reviewed
    /// destination may stage the derived projection only while the record
    /// revision named by the sealed proposal is still current and undeleted.
    pub async fn runtime_contribution_source_snapshot(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        entity: &AppName,
        record_id: &AppRecordId,
        record_revision: AppRevision,
        now: DateTime<Utc>,
    ) -> Result<AppRuntimeAuthoritySnapshot, AppEntityStoreError> {
        let installation_id = installation_id.clone();
        let entity = entity.clone();
        let record_id = record_id.clone();
        self.registry
            .execute_scoped_typed_read(authenticated_scope, &now, move |connection, scope| {
                let transaction = connection.unchecked_transaction()?;
                let installation =
                    load_installation_in_snapshot(&transaction, scope, &installation_id)?
                        .ok_or(AppEntityStoreError::MissingInstallation)?;
                let active = resolve_active_schema(&transaction, scope, &installation_id)?
                    .ok_or(AppEntityStoreError::MissingInstallation)?;
                if installation.lifecycle.generation != active.installation_generation()
                    || installation.package_revision_ref != *active.package_revision_ref()
                    || installation.grant_revision != Some(active.grant_revision())
                    || installation.active_schema_revision != Some(active.schema_revision())
                    || installation.active_surface_revision != active.active_surface_revision()
                    || active.runtime_contract(&entity).is_none()
                {
                    return Err(AppEntityStoreError::StaleSchemaBinding);
                }
                let mut decoded_bytes = 0usize;
                let record = load_current_record_by_id(
                    &transaction,
                    &installation_id,
                    &entity,
                    &record_id,
                    &mut decoded_bytes,
                )?
                .ok_or(AppEntityStoreError::StaleRecordProjection)?;
                if record.record_revision != record_revision {
                    return Err(AppEntityStoreError::StaleRecordProjection);
                }
                let source_record_digest = AppDigest::blake3_canonical_json(&record.payload)?;
                transaction.commit()?;
                Ok(AppRuntimeAuthoritySnapshot {
                    installation,
                    active,
                    source_record_digest: Some(source_record_digest),
                    source_record_payload: Some(record.payload),
                })
            })
            .await?
            .ok_or(AppEntityStoreError::MissingScopedStore)
    }

    /// Resolve the installation/schema/package tuple **without** requiring the
    /// installation to be enabled.
    ///
    /// Same rule as [`resolve_data_owner_schema`]: a disabled, parked or
    /// retained app loses *execution* authority, not the owner's ability to
    /// look at it. Owner review needs exactly this — an update review happens
    /// while the installation sits in `UpdatePending`, and a reinstall review
    /// while it sits in `UninstalledRetained`, so the enabled-only resolver
    /// would refuse the very cases that need to read the grant already in
    /// force. Purged installations remain unavailable.
    ///
    /// Never use this to authorise execution.
    pub async fn data_owner_schema(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Option<ActiveAppEntitySchema>, AppEntityStoreError> {
        let installation_id = installation_id.clone();
        let resolved = self
            .registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(resolve_data_owner_schema(
                    connection,
                    scope,
                    &installation_id,
                ))
            })
            .await?;
        match resolved {
            None => Ok(None),
            Some(result) => result,
        }
    }

    /// Execute one exact Phase-0 query contract. Reads are bounded and use a
    /// server-held revision snapshot; only opaque cursor references leave the
    /// service. Cursor persistence is a short registry-owned write performed
    /// after the read snapshot, with the active installation tuple rechecked.
    pub async fn query(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppStoreAuthorityFence,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppQueryPage, AppEntityStoreError> {
        Ok(self
            .query_with_policy(authenticated_scope, authority, request, now)
            .await?
            .page)
    }

    /// Execute the canonical query while retaining the trusted policy result
    /// for the owner adapter. Ordinary store callers continue using `query`.
    pub async fn query_with_policy(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppStoreAuthorityFence,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedQueryPage, AppEntityStoreError> {
        self.query_with_policy_mode(authenticated_scope, authority, request, now, true)
            .await
    }

    /// A native context consumer needs one current page, never a continuation.
    /// Keep the normal authority and policy path but retain no abandoned cursor.
    pub(crate) async fn query_page_without_continuation(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppStoreAuthorityFence,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedQueryPage, AppEntityStoreError> {
        self.query_with_policy_mode(authenticated_scope, authority, request, now, false)
            .await
    }

    async fn query_with_policy_mode(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppStoreAuthorityFence,
        request: AppQueryRequest,
        now: DateTime<Utc>,
        retain_continuation: bool,
    ) -> Result<AppGovernedQueryPage, AppEntityStoreError> {
        let scope_binding_ref = authenticated_scope.scope_binding_ref().clone();
        let authentication_revision = authenticated_scope.authentication_revision();
        let read_request = request.clone();
        let mut prepared = self
            .registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(prepare_query_page(
                    connection,
                    scope,
                    &scope_binding_ref,
                    authentication_revision,
                    authority,
                    &read_request,
                    now,
                ))
            })
            .await?
            .ok_or(AppEntityStoreError::MissingScopedStore)??;

        if !retain_continuation {
            prepared.cursor_write = None;
            prepared.page.next_cursor = None;
        }
        if let Some(cursor_write) = prepared.cursor_write {
            self.persist_cursor(authenticated_scope, cursor_write, now)
                .await?;
        }
        Ok(AppGovernedQueryPage {
            page: prepared.page,
            handling_policy: prepared.handling_policy,
        })
    }

    /// Re-open one exact query-page row under a fresh personal-agent store
    /// fence. The operation compares the selected values and record revision
    /// with the current record head in the same read snapshot that consumes
    /// the current installation/grant/schema authority.
    pub async fn revalidate_record_projection(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppStoreAuthorityFence,
        request: AppQueryRequest,
        expected_envelope: &AppDataEnvelope<Vec<AppRecordProjection>>,
        expected_projection: &AppRecordProjection,
        now: DateTime<Utc>,
    ) -> Result<AppRevalidatedRecordProjection, AppEntityStoreError> {
        let scope_binding_ref = authenticated_scope.scope_binding_ref().clone();
        let authentication_revision = authenticated_scope.authentication_revision();
        let expected_envelope = expected_envelope.clone();
        let expected_projection = expected_projection.clone();
        let resolved = self
            .registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(revalidate_record_projection_in_snapshot(
                    connection,
                    scope,
                    &scope_binding_ref,
                    authentication_revision,
                    authority,
                    &request,
                    &expected_envelope,
                    &expected_projection,
                    now,
                ))
            })
            .await?
            .ok_or(AppEntityStoreError::MissingScopedStore)??;
        Ok(resolved)
    }

    /// Revalidate every row of one bounded page in a single read transaction.
    /// Each row still consumes its own freshly minted authority fence and runs
    /// the exact revision, selected-byte and policy checks used for one row.
    /// This avoids reopening the registry/keychain once per returned record.
    pub(crate) async fn revalidate_record_projections(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authorities: Vec<AppStoreAuthorityFence>,
        request: AppQueryRequest,
        expected_envelope: &AppDataEnvelope<Vec<AppRecordProjection>>,
        now: DateTime<Utc>,
    ) -> Result<(), AppEntityStoreError> {
        request.validate_app_contract(&AppContractLimits::default())?;
        if authorities.len() != expected_envelope.value.len()
            || authorities.len() > request.limit as usize
        {
            return Err(AppEntityStoreError::InvalidRecordProjection);
        }
        let scope_binding_ref = authenticated_scope.scope_binding_ref().clone();
        let authentication_revision = authenticated_scope.authentication_revision();
        let expected_envelope = expected_envelope.clone();
        self.registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                let transaction = connection.unchecked_transaction()?;
                for (authority, projection) in authorities.into_iter().zip(&expected_envelope.value)
                {
                    if let Err(error) = revalidate_record_projection_in_snapshot(
                        &transaction,
                        scope,
                        &scope_binding_ref,
                        authentication_revision,
                        authority,
                        &request,
                        &expected_envelope,
                        projection,
                        now,
                    ) {
                        return Ok(Err(error));
                    }
                }
                transaction.commit()?;
                Ok(Ok(()))
            })
            .await?
            .ok_or(AppEntityStoreError::MissingScopedStore)??;
        Ok(())
    }

    /// Revalidate either supported brokered-source evidence shape at one
    /// registry snapshot. Action-result bytes and task/result sidecars are
    /// reopened by the workflow owner; this store check owns only the live
    /// installation/grant tuple so copied receipt JSON is never authority.
    pub(crate) async fn revalidate_composition_source_receipt(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        receipt: &AppCompositionTransferReceipt,
        now: DateTime<Utc>,
    ) -> Result<(), AppEntityStoreError> {
        let receipt = receipt.clone();
        self.registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(revalidate_composition_source_in_snapshot(
                    connection, scope, &receipt,
                ))
            })
            .await?
            .ok_or(AppEntityStoreError::MissingScopedStore)??;
        Ok(())
    }

    async fn persist_cursor(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        cursor: PendingCursorWrite,
        now: DateTime<Utc>,
    ) -> Result<(), AppEntityStoreError> {
        let result = self
            .registry
            .execute_scoped_write(authenticated_scope, &now, move |connection, scope| {
                Ok(persist_cursor_blocking(connection, scope, cursor, now))
            })
            .await?;
        result
    }
}

fn revalidate_composition_source_in_snapshot(
    connection: &Connection,
    scope: &AppScope,
    receipt: &AppCompositionTransferReceipt,
) -> Result<(), AppEntityStoreError> {
    match (
        receipt.source_records.as_slice(),
        receipt.source_action_result.as_ref(),
    ) {
        (records, None) if !records.is_empty() => {
            revalidate_composition_source_records_in_snapshot(
                connection,
                scope,
                &receipt.source_installation_id,
                receipt.source_installation_generation,
                &receipt.source_package_revision_ref,
                receipt.source_schema_revision,
                receipt.source_grant_revision,
                records,
                &receipt.source_handling_policy_digest,
            )
        },
        ([], Some(fence)) => {
            if fence.fields.len() > AppContractLimits::default().max_collection_items()
                || !fence.fields.windows(2).all(|fields| fields[0] < fields[1])
            {
                return Err(AppEntityStoreError::StaleRecordProjection);
            }
            let active = resolve_active_schema(connection, scope, &receipt.source_installation_id)?
                .ok_or(AppEntityStoreError::MissingInstallation)?;
            if active.installation_generation() != receipt.source_installation_generation
                || active.package_revision_ref() != &receipt.source_package_revision_ref
                || active.schema_revision() != receipt.source_schema_revision
                || active.grant_revision() != receipt.source_grant_revision
                || active.grant().revoked_at.is_some()
                || active.grant().granted_data_handling_policy_digest
                    != fence.source_grant_policy_digest
                || active
                    .grant()
                    .granted_data_handling_policy
                    .personal_agent_access
                    != AppPersonalAgentAccess::ApprovedProjection
            {
                return Err(AppEntityStoreError::StaleRecordProjection);
            }
            Ok(())
        },
        _ => Err(AppEntityStoreError::StaleRecordProjection),
    }
}

#[allow(clippy::too_many_arguments)]
fn revalidate_composition_source_records_in_snapshot(
    connection: &Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    installation_generation: u64,
    package_revision_ref: &AppReference,
    schema_revision: AppRevision,
    grant_revision: AppRevision,
    source_records: &[AppSourceRecordFence],
    expected_policy_digest: &AppDigest,
) -> Result<(), AppEntityStoreError> {
    if source_records.is_empty()
        || source_records.len() > AppContractLimits::default().max_collection_items()
    {
        return Err(AppEntityStoreError::StaleRecordProjection);
    }
    let active = resolve_active_schema(connection, scope, installation_id)?
        .ok_or(AppEntityStoreError::MissingInstallation)?;
    if active.installation_generation() != installation_generation
        || active.package_revision_ref() != package_revision_ref
        || active.schema_revision() != schema_revision
        || active.grant_revision() != grant_revision
        || active
            .grant()
            .granted_data_handling_policy
            .personal_agent_access
            != AppPersonalAgentAccess::ApprovedProjection
    {
        return Err(AppEntityStoreError::StaleRecordProjection);
    }

    let mut effective_policy: Option<AppDataHandlingPolicy> = None;
    let mut decoded_bytes = 0usize;
    let mut identities = BTreeSet::new();
    for fence in source_records {
        if fence.fields.len() > AppContractLimits::default().max_collection_items()
            || !fence.fields.windows(2).all(|fields| fields[0] < fields[1])
            || fence.policy_influence_fields.is_empty()
            || fence.policy_influence_fields.len()
                > AppContractLimits::default().max_collection_items()
            || !fence
                .policy_influence_fields
                .windows(2)
                .all(|fields| fields[0] < fields[1])
            || !fence
                .fields
                .iter()
                .all(|field| fence.policy_influence_fields.contains(field))
            || !identities.insert((fence.entity.clone(), fence.record_id.clone()))
        {
            return Err(AppEntityStoreError::StaleRecordProjection);
        }
        let runtime = active
            .runtime_contract(&fence.entity)
            .ok_or_else(|| AppEntityStoreError::UnknownEntity(fence.entity.to_string()))?;
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: installation_id.clone(),
            entity: fence.entity.clone(),
            select: fence.policy_influence_fields.clone(),
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit: 1,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("app_composition")?,
        };
        validate_typed_query(&request, runtime.query_schema())?;
        let selected = selected_policy(&active, &request, runtime)?;
        let current = load_current_record_by_id(
            connection,
            installation_id,
            &fence.entity,
            &fence.record_id,
            &mut decoded_bytes,
        )?
        .ok_or(AppEntityStoreError::StaleRecordProjection)?;
        runtime.validate_payload(&current.payload)?;
        if current.record_revision != fence.record_revision {
            return Err(AppEntityStoreError::StaleRecordProjection);
        }
        let mut selected_values = BTreeMap::new();
        for path in &fence.fields {
            if let Some(value) = current.payload.get(path.as_str()) {
                selected_values.insert(path.clone(), value.clone());
            }
        }
        if AppDigest::blake3_canonical_json(&serde_json::to_value(&selected_values)?)?
            != fence.selected_values_digest
        {
            return Err(AppEntityStoreError::StaleRecordProjection);
        }
        let record_policy = join_handling_policy(selected, current.handling_policy);
        if record_policy.personal_agent_access != AppPersonalAgentAccess::ApprovedProjection {
            return Err(AppEntityStoreError::PersonalAgentPolicyDenied);
        }
        effective_policy = Some(match effective_policy {
            Some(policy) => join_handling_policy(policy, record_policy),
            None => record_policy,
        });
    }
    let effective_policy = effective_policy.ok_or(AppEntityStoreError::StaleRecordProjection)?;
    if AppDigest::blake3_canonical_json(&serde_json::to_value(&effective_policy)?)?
        != *expected_policy_digest
    {
        return Err(AppEntityStoreError::StaleRecordProjection);
    }
    Ok(())
}

fn load_installation_in_snapshot(
    connection: &Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
) -> Result<Option<AppInstallation>, AppEntityStoreError> {
    let bytes = connection
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let installation: AppInstallation = decode_contract(&bytes)?;
    if installation.scope != *scope || installation.installation_id != *installation_id {
        return Err(AppEntityStoreError::ScopeOrIdentityMismatch);
    }
    Ok(Some(installation))
}

struct PreparedQueryPage {
    page: AppQueryPage,
    handling_policy: AppDataHandlingPolicy,
    cursor_write: Option<PendingCursorWrite>,
}

struct PendingCursorWrite {
    keyset: bool,
    installation_id: AppInstallationId,
    installation_generation: u64,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    dataset_generation: u64,
    source_cursor_ref: Option<AppReference>,
    new_cursor_ref: Option<AppReference>,
    snapshot_ref: AppReference,
    evidence_json: Option<Vec<u8>>,
    snapshot_json: Vec<u8>,
    next_offset: usize,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SnapshotEntry {
    record_id: AppRecordId,
    record_revision: AppRevision,
}

#[derive(Clone)]
struct LoadedRecord {
    record_id: AppRecordId,
    record_revision: AppRevision,
    payload: Value,
    handling_policy: AppDataHandlingPolicy,
}

fn prepare_query_page(
    connection: &Connection,
    scope: &AppScope,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    authentication_revision: AppRevision,
    authority: AppStoreAuthorityFence,
    request: &AppQueryRequest,
    now: DateTime<Utc>,
) -> Result<PreparedQueryPage, AppEntityStoreError> {
    let transaction = connection.unchecked_transaction()?;
    let prepared = prepare_query_page_in_snapshot(
        &transaction,
        scope,
        scope_binding_ref,
        authentication_revision,
        authority,
        request,
        now,
    )?;
    transaction.commit()?;
    Ok(prepared)
}

#[allow(clippy::too_many_arguments)]
fn revalidate_record_projection_in_snapshot(
    connection: &Connection,
    scope: &AppScope,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    authentication_revision: AppRevision,
    authority: AppStoreAuthorityFence,
    request: &AppQueryRequest,
    expected_envelope: &AppDataEnvelope<Vec<AppRecordProjection>>,
    expected_projection: &AppRecordProjection,
    now: DateTime<Utc>,
) -> Result<AppRevalidatedRecordProjection, AppEntityStoreError> {
    request.validate_app_contract(&AppContractLimits::default())?;
    let active = resolve_active_schema(connection, scope, &request.source_installation_id)?
        .ok_or(AppEntityStoreError::MissingInstallation)?;
    let read_audience = authority.consume_for_active_query(
        request,
        scope_binding_ref,
        authentication_revision,
        active.installation_id(),
        active.installation_generation(),
        active.package_revision_ref(),
        active.grant_revision(),
        active.schema_revision(),
        active.active_surface_revision(),
    )?;
    if expected_envelope.protocol_version != AppProtocolVersion::V1
        || expected_envelope.source != AppDataSource::AppStore
        || &expected_envelope.scope_binding_ref != scope_binding_ref
        || expected_envelope.installation_id != *active.installation_id()
        || expected_envelope.package_revision_ref != *active.package_revision_ref()
        || expected_envelope.schema_revision != active.schema_revision()
        || expected_envelope.grant_revision != active.grant_revision()
        || expected_envelope.produced_at > now
        || expected_envelope
            .expires_at
            .is_some_and(|expires_at| now >= expires_at)
    {
        return Err(AppEntityStoreError::StaleRecordProjection);
    }
    let expected_content_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&expected_envelope.value)?)?;
    let expected_provenance_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&expected_envelope.source_refs)?)?;
    if expected_envelope.content_digest != expected_content_digest
        || expected_envelope.handling_labels.provenance_digest != expected_provenance_digest
        || expected_envelope
            .value
            .iter()
            .filter(|projection| {
                projection.entity == expected_projection.entity
                    && projection.record_id == expected_projection.record_id
            })
            .count()
            != 1
        || !expected_envelope
            .value
            .iter()
            .any(|projection| projection == expected_projection)
        || request.entity != expected_projection.entity
    {
        return Err(AppEntityStoreError::InvalidRecordProjection);
    }

    let runtime = active
        .runtime_contract(&request.entity)
        .ok_or_else(|| AppEntityStoreError::UnknownEntity(request.entity.to_string()))?;
    validate_typed_query(request, runtime.query_schema())?;
    let selected_handling_policy = selected_policy(&active, request, runtime)?;
    let mut decoded_bytes = 0usize;
    let current = load_current_record_by_id(
        connection,
        active.installation_id(),
        &request.entity,
        &expected_projection.record_id,
        &mut decoded_bytes,
    )?
    .ok_or(AppEntityStoreError::StaleRecordProjection)?;
    runtime.validate_payload(&current.payload)?;
    if current.record_revision != expected_projection.record_revision {
        return Err(AppEntityStoreError::StaleRecordProjection);
    }

    let mut fields = BTreeMap::new();
    for path in &request.select {
        if let Some(value) = current.payload.get(path.as_str()) {
            fields.insert(path.clone(), value.clone());
        }
    }
    if fields != expected_projection.fields {
        return Err(AppEntityStoreError::StaleRecordProjection);
    }
    let projection = AppRecordProjection {
        entity: request.entity.clone(),
        record_id: current.record_id.clone(),
        record_revision: current.record_revision,
        fields,
    };
    let mut source_refs = Vec::with_capacity(1);
    push_source_ref(
        &mut source_refs,
        &request.entity,
        &current,
        request.select.clone(),
    )?;
    let source_ref = source_refs
        .pop()
        .ok_or(AppEntityStoreError::InvalidRecordProjection)?;
    if !expected_envelope.source_refs.iter().any(|candidate| {
        candidate.kind == source_ref.kind
            && candidate.reference == source_ref.reference
            && candidate.revision == source_ref.revision
            && source_ref
                .fields
                .iter()
                .all(|field| candidate.fields.contains(field))
    }) {
        return Err(AppEntityStoreError::InvalidRecordProjection);
    }
    let handling_policy = join_handling_policy(selected_handling_policy, current.handling_policy);
    ensure_read_audience_policy(&read_audience, &handling_policy)?;
    Ok(AppRevalidatedRecordProjection {
        active,
        projection,
        source_ref,
        handling_policy,
        read_audience,
    })
}

#[allow(clippy::too_many_arguments)]
fn prepare_query_page_in_snapshot(
    connection: &Connection,
    scope: &AppScope,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    authentication_revision: AppRevision,
    authority: AppStoreAuthorityFence,
    request: &AppQueryRequest,
    now: DateTime<Utc>,
) -> Result<PreparedQueryPage, AppEntityStoreError> {
    let active = resolve_active_schema(connection, scope, &request.source_installation_id)?
        .ok_or(AppEntityStoreError::MissingInstallation)?;
    let read_audience = authority.consume_for_active_query(
        request,
        scope_binding_ref,
        authentication_revision,
        active.installation_id(),
        active.installation_generation(),
        active.package_revision_ref(),
        active.grant_revision(),
        active.schema_revision(),
        active.active_surface_revision(),
    )?;
    let runtime = active
        .runtime_contract(&request.entity)
        .ok_or_else(|| AppEntityStoreError::UnknownEntity(request.entity.to_string()))?;
    validate_typed_query(request, runtime.query_schema())?;
    let selected_handling_policy = selected_policy(&active, request, runtime)?;
    ensure_read_audience_policy(&read_audience, &selected_handling_policy)?;
    let current_generation = current_dataset_generation(connection, active.installation_id())?;
    let mut dataset_generation = current_generation;
    let keyset = request.pagination == super::models::AppQueryPagination::Keyset;
    let mut keyset_boundary = None;
    let mut keyset_chain = None;

    let (snapshot, offset, cursor_window, retained_snapshot_ref, mut loaded) = if keyset {
        let (after, chain) = read_keyset_cursor(
            connection,
            request,
            runtime,
            &active,
            current_generation,
            now,
        )?;
        keyset_chain = chain;
        let Some(predicate) =
            indexed_query_filter(connection, active.installation_id(), request, runtime)?
        else {
            return Err(AppEntityStoreError::KeysetIndexRequired);
        };
        if request.order.len() > 1
            || request.order.iter().any(|order| {
                !runtime
                    .field(&order.field)
                    .is_some_and(|field| field.indexed())
            })
        {
            return Err(AppEntityStoreError::KeysetIndexRequired);
        }
        let order = request
            .order
            .first()
            .map(|order| super::indexed_snapshot::Order {
                field: order.field.to_string(),
                descending: order.direction == AppOrderDirection::Descending,
            });
        let required = request
            .order
            .first()
            .and_then(|order| runtime.field(&order.field))
            .is_some_and(|field| field.required());
        let rows = super::indexed_snapshot::read_keyset(
            connection,
            active.installation_id().as_str(),
            request.entity.as_str(),
            predicate.as_ref(),
            order.as_ref(),
            required,
            after.as_ref(),
            request.limit as usize,
            MAX_QUERY_SCAN_BYTES,
        )
        .map_err(|error| match &error {
            rusqlite::Error::SqliteFailure(code, _)
                if code.extended_code == rusqlite::ffi::SQLITE_TOOBIG =>
            {
                AppEntityStoreError::QueryScanTooLarge
            },
            _ => error.into(),
        })?;
        let end = rows.len().min(request.limit as usize);
        keyset_boundary = rows
            .get(end.saturating_sub(1))
            .map(|row| row.boundary.clone());
        let snapshot = rows
            .into_iter()
            .map(|row| {
                Ok(SnapshotEntry {
                    record_id: AppRecordId::parse(row.boundary.record_id)?,
                    record_revision: AppRevision::new(
                        u64::try_from(row.revision)
                            .map_err(|_| AppEntityStoreError::InvalidRevision)?,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, AppEntityStoreError>>()?;
        let loaded = load_snapshot_records(
            connection,
            active.installation_id(),
            &request.entity,
            &snapshot[..end],
        )?;
        validate_loaded_records(runtime, &loaded)?;
        (snapshot, 0, None, None, loaded)
    } else if let Some(cursor_ref) = &request.cursor {
        let (evidence_json, snapshot_json, next_offset, snapshot_ref, snapshot_generation): (
            Vec<u8>,
            Option<Vec<u8>>,
            i64,
            Option<String>,
            Option<i64>,
        ) = connection
            .query_row(
                "SELECT cursor.evidence_json, snapshot.snapshot_json,
                        cursor.next_offset, cursor.snapshot_ref, snapshot.dataset_generation
                   FROM app_query_cursors AS cursor
                   LEFT JOIN app_query_cursors AS snapshot
                     ON snapshot.cursor_ref = cursor.snapshot_ref
                    AND snapshot.installation_id = cursor.installation_id
                    AND snapshot.snapshot_ref = snapshot.cursor_ref
                  WHERE cursor.cursor_ref = ?1 AND cursor.installation_id = ?2",
                params![cursor_ref.as_str(), active.installation_id().as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(AppEntityStoreError::MissingCursor)?;
        let snapshot_json = snapshot_json.ok_or(AppEntityStoreError::CorruptCursor)?;
        let snapshot_ref =
            AppReference::parse(snapshot_ref.ok_or(AppEntityStoreError::CorruptCursor)?)?;
        // Membership belongs to the retained snapshot, not the latest
        // dataset head. Appends and unrelated edits must not invalidate
        // pagination. Each returned row still has to match its live head
        // below, under the current grant/schema and selected-field policy.
        dataset_generation =
            u64::try_from(snapshot_generation.ok_or(AppEntityStoreError::CorruptCursor)?)
                .map_err(|_| AppEntityStoreError::CorruptCursor)?;
        if dataset_generation > current_generation {
            return Err(AppEntityStoreError::StaleDatasetGeneration);
        }
        let validated = AppQueryCursorEvidence::decode_and_validate_trusted_store(
            &evidence_json,
            request,
            runtime.query_schema(),
            active.schema_revision(),
            dataset_generation,
            &now,
        )?;
        let snapshot = decode_snapshot(&snapshot_json)?;
        let offset =
            usize::try_from(next_offset).map_err(|_| AppEntityStoreError::CorruptCursor)?;
        if offset == 0
            || offset > snapshot.len()
            || snapshot[offset - 1].record_id != *validated.last_record_id()
            || validated.dataset_generation() != dataset_generation
        {
            return Err(AppEntityStoreError::CorruptCursor);
        }
        let end = offset
            .saturating_add(usize::try_from(request.limit).unwrap_or(usize::MAX))
            .min(snapshot.len());
        let loaded = load_snapshot_records(
            connection,
            active.installation_id(),
            &request.entity,
            &snapshot[offset..end],
        )?;
        validate_loaded_records(runtime, &loaded)?;
        (
            snapshot,
            offset,
            Some((validated.issued_at(), validated.expires_at())),
            Some(snapshot_ref),
            loaded,
        )
    } else if let Some(snapshot) =
        indexed_query_snapshot(connection, active.installation_id(), request, runtime)?
    {
        let end = usize::try_from(request.limit)
            .unwrap_or(usize::MAX)
            .min(snapshot.len());
        let loaded = load_snapshot_records(
            connection,
            active.installation_id(),
            &request.entity,
            &snapshot[..end],
        )?;
        validate_loaded_records(runtime, &loaded)?;
        (snapshot, 0, None, None, loaded)
    } else {
        let indexed = if dataset_indexes_are_complete(connection, active.installation_id())? {
            request
                .predicate
                .as_ref()
                .map(|predicate| {
                    load_indexed_current_records(
                        connection,
                        active.installation_id(),
                        &request.entity,
                        predicate,
                        runtime,
                    )
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        let mut loaded = match indexed {
            Some(records) => records,
            None => load_current_records(connection, active.installation_id(), &request.entity)?,
        };
        validate_loaded_records(runtime, &loaded)?;
        if let Some(predicate) = &request.predicate {
            let mut filtered = Vec::with_capacity(loaded.len());
            for record in loaded {
                if evaluate_predicate(predicate, runtime, &record.payload)? {
                    filtered.push(record);
                }
            }
            loaded = filtered;
        }
        loaded.sort_by(|left, right| compare_records(left, right, request, runtime));
        let snapshot = loaded
            .iter()
            .map(|record| SnapshotEntry {
                record_id: record.record_id.clone(),
                record_revision: record.record_revision,
            })
            .collect::<Vec<_>>();
        let end = usize::try_from(request.limit)
            .unwrap_or(usize::MAX)
            .min(loaded.len());
        loaded.truncate(end);
        (snapshot, 0, None, None, loaded)
    };

    let page_start = if request.cursor.is_some() { offset } else { 0 };
    let next_offset = page_start.saturating_add(loaded.len());
    let mut source_refs = Vec::new();
    let mut record_policies = Vec::new();
    let mut relation_rows_remaining = AppContractLimits::default()
        .max_collection_items()
        .saturating_sub(loaded.len());
    let mut relation_decoded_bytes = 0usize;
    let mut projections = Vec::with_capacity(loaded.len());
    let mut relation_cache = BTreeMap::new();
    for record in loaded.drain(..) {
        projections.push(project_record(
            connection,
            &active,
            request,
            runtime,
            record,
            &mut source_refs,
            &mut record_policies,
            &mut relation_rows_remaining,
            &mut relation_cache,
            &mut relation_decoded_bytes,
        )?);
    }

    let has_more = next_offset < snapshot.len();
    let (issued_at, expires_at) = cursor_window.unwrap_or((
        now,
        now + chrono::Duration::from_std(if keyset {
            KEYSET_CURSOR_TTL
        } else {
            QUERY_CURSOR_TTL
        })
        .map_err(|_| AppEntityStoreError::InvalidCursorExpiry)?,
    ));
    let (next_cursor, evidence_json) = if has_more {
        let last_record_id = projections
            .last()
            .map(|projection| projection.record_id.clone())
            .ok_or(AppEntityStoreError::CorruptCursor)?;
        let evidence = AppQueryCursorEvidence::mint(
            request,
            runtime.query_schema(),
            active.schema_revision(),
            dataset_generation,
            last_record_id,
            issued_at,
            expires_at,
        )?;
        (
            Some(evidence.cursor_ref().clone()),
            Some(evidence.encode_trusted_store()?),
        )
    } else {
        (None, None)
    };
    let (page, handling_policy) = build_query_page(
        scope_binding_ref,
        &active,
        request,
        projections,
        source_refs,
        record_policies,
        selected_handling_policy,
        next_cursor.clone(),
        &read_audience,
        now,
    )?;
    let cursor_write = if next_cursor.is_some() {
        let new_cursor_ref = next_cursor
            .clone()
            .ok_or(AppEntityStoreError::CorruptCursor)?;
        let source_cursor_ref = request.cursor.clone();
        let snapshot_ref = keyset_chain
            .or(retained_snapshot_ref)
            .unwrap_or_else(|| new_cursor_ref.clone());
        let snapshot_json = if keyset {
            serde_json::to_vec(&keyset_boundary.ok_or(AppEntityStoreError::CorruptCursor)?)?
        } else if source_cursor_ref.is_some() {
            Vec::new()
        } else {
            encode_snapshot(&snapshot)?
        };
        Some(PendingCursorWrite {
            keyset,
            installation_id: active.installation_id().clone(),
            installation_generation: active.installation_generation(),
            package_revision_ref: active.package_revision_ref().clone(),
            schema_revision: active.schema_revision(),
            dataset_generation,
            source_cursor_ref,
            new_cursor_ref: Some(new_cursor_ref),
            snapshot_ref,
            evidence_json,
            snapshot_json,
            next_offset,
            expires_at,
        })
    } else {
        None
    };
    Ok(PreparedQueryPage {
        page,
        handling_policy,
        cursor_write,
    })
}

fn read_keyset_cursor(
    connection: &Connection,
    request: &AppQueryRequest,
    runtime: &AppEntityRuntimeContract,
    active: &ActiveAppEntitySchema,
    current_generation: u64,
    now: DateTime<Utc>,
) -> Result<
    (
        Option<super::indexed_snapshot::Boundary>,
        Option<AppReference>,
    ),
    AppEntityStoreError,
> {
    let Some(cursor_ref) = &request.cursor else {
        return Ok((None, None));
    };
    let (evidence, boundary, chain, generation, installation_generation, package, schema): (Vec<u8>, Vec<u8>, String, i64, i64, String, i64) = connection.query_row(
        "SELECT evidence_json, boundary_json, chain_ref, dataset_generation, installation_generation, package_revision_ref, schema_revision
         FROM app_keyset_cursors WHERE cursor_ref = ?1 AND installation_id = ?2",
        params![cursor_ref.as_str(), active.installation_id().as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    ).optional()?.ok_or(AppEntityStoreError::MissingCursor)?;
    if u64::try_from(installation_generation).ok() != Some(active.installation_generation())
        || package != active.package_revision_ref().as_str()
        || u64::try_from(schema).ok() != Some(active.schema_revision().get())
    {
        return Err(AppEntityStoreError::StaleSchemaBinding);
    }
    let generation = u64::try_from(generation).map_err(|_| AppEntityStoreError::CorruptCursor)?;
    if generation > current_generation {
        return Err(AppEntityStoreError::StaleDatasetGeneration);
    }
    let validated = AppQueryCursorEvidence::decode_and_validate_trusted_store(
        &evidence,
        request,
        runtime.query_schema(),
        active.schema_revision(),
        generation,
        &now,
    )?;
    let boundary: super::indexed_snapshot::Boundary = serde_json::from_slice(&boundary)?;
    if boundary.record_id != validated.last_record_id().as_str() {
        return Err(AppEntityStoreError::CorruptCursor);
    }
    Ok((Some(boundary), Some(AppReference::parse(chain)?)))
}

fn persist_keyset_cursor(
    connection: &mut Connection,
    scope: &AppScope,
    cursor: PendingCursorWrite,
    now: DateTime<Utc>,
) -> Result<(), AppEntityStoreError> {
    let transaction = connection.transaction()?;
    let active = resolve_active_schema(&transaction, scope, &cursor.installation_id)?
        .ok_or(AppEntityStoreError::MissingInstallation)?;
    if active.installation_generation() != cursor.installation_generation
        || active.package_revision_ref() != &cursor.package_revision_ref
        || active.schema_revision() != cursor.schema_revision
    {
        return Err(AppEntityStoreError::StaleSchemaBinding);
    }
    if current_dataset_generation(&transaction, &cursor.installation_id)?
        < cursor.dataset_generation
    {
        return Err(AppEntityStoreError::StaleDatasetGeneration);
    }
    let cursor_ref = cursor
        .new_cursor_ref
        .ok_or(AppEntityStoreError::CorruptCursor)?;
    let evidence = cursor
        .evidence_json
        .ok_or(AppEntityStoreError::CorruptCursor)?;
    // Bounds storage by active traversals, not by pages already visited.
    transaction.execute(
        "DELETE FROM app_keyset_cursors WHERE cursor_ref IN
        (SELECT cursor_ref FROM app_keyset_cursors WHERE expires_at <= ?1 LIMIT 128)",
        [now.to_rfc3339()],
    )?;
    transaction.execute(
        "DELETE FROM app_keyset_cursors WHERE installation_id = ?1 AND expires_at <= ?2",
        params![cursor.installation_id.as_str(), now.to_rfc3339()],
    )?;
    if let Some(source) = &cursor.source_cursor_ref {
        let valid: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM app_keyset_cursors
            WHERE cursor_ref = ?1 AND installation_id = ?2 AND chain_ref = ?3 AND schema_revision = ?4
            AND installation_generation = ?5 AND package_revision_ref = ?6)",
            params![source.as_str(), cursor.installation_id.as_str(), cursor.snapshot_ref.as_str(), cursor.schema_revision.get(),
                cursor.installation_generation, cursor.package_revision_ref.as_str()], |row| row.get(0))?;
        if !valid {
            return Err(AppEntityStoreError::MissingCursor);
        }
        transaction.execute("DELETE FROM app_keyset_cursors WHERE installation_id = ?1 AND chain_ref = ?2 AND cursor_ref <> ?3 AND cursor_ref <> ?4",
            params![cursor.installation_id.as_str(), cursor.snapshot_ref.as_str(), source.as_str(), cursor_ref.as_str()])?;
    }
    let existing: Option<(Vec<u8>, Vec<u8>)> = transaction
        .query_row(
            "SELECT evidence_json, boundary_json FROM app_keyset_cursors WHERE cursor_ref = ?1",
            [cursor_ref.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != (evidence, cursor.snapshot_json) {
            return Err(AppEntityStoreError::CorruptCursor);
        }
        transaction.commit()?;
        return Ok(());
    }
    if !super::indexed_snapshot::reserve_keyset_cursor_capacity(
        &transaction,
        cursor.installation_id.as_str(),
        cursor.snapshot_ref.as_str(),
        (evidence.len() + cursor.snapshot_json.len()) as i64,
        MAX_QUERY_CURSORS_PER_INSTALLATION,
        MAX_QUERY_CURSOR_BYTES_PER_INSTALLATION,
    )? {
        return Err(AppEntityStoreError::CursorCapacityExceeded);
    }
    transaction.execute("INSERT INTO app_keyset_cursors(cursor_ref, installation_id, installation_generation, package_revision_ref,
        schema_revision, dataset_generation, evidence_json, boundary_json, chain_ref, expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![cursor_ref.as_str(), cursor.installation_id.as_str(), cursor.installation_generation, cursor.package_revision_ref.as_str(),
            cursor.schema_revision.get(), cursor.dataset_generation, evidence, cursor.snapshot_json, cursor.snapshot_ref.as_str(), cursor.expires_at.to_rfc3339()])?;
    transaction.commit()?;
    Ok(())
}

fn persist_cursor_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    cursor: PendingCursorWrite,
    now: DateTime<Utc>,
) -> Result<(), AppEntityStoreError> {
    let active = resolve_active_schema(connection, scope, &cursor.installation_id)?
        .ok_or(AppEntityStoreError::MissingInstallation)?;
    if active.installation_generation() != cursor.installation_generation
        || active.package_revision_ref() != &cursor.package_revision_ref
        || active.schema_revision() != cursor.schema_revision
    {
        return Err(AppEntityStoreError::StaleSchemaBinding);
    }
    if cursor.keyset {
        return persist_keyset_cursor(connection, scope, cursor, now);
    }
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM app_query_cursors
         WHERE snapshot_ref IN (
             SELECT snapshot_ref FROM app_query_cursors
              WHERE expires_at <= ?1
              GROUP BY snapshot_ref
              ORDER BY MIN(expires_at) ASC
              LIMIT 128
         )",
        params![now.to_rfc3339()],
    )?;
    transaction.execute(
        "DELETE FROM app_query_cursors
          WHERE installation_id = ?1 AND expires_at <= ?2",
        params![cursor.installation_id.as_str(), now.to_rfc3339()],
    )?;
    let current_generation = current_dataset_generation(&transaction, &cursor.installation_id)?;
    // A write after the read snapshot cannot invalidate the page we just
    // prepared. Retain its original generation; continuation revalidates the
    // exact live record heads. A rollback/replacement is still rejected.
    if current_generation < cursor.dataset_generation {
        return Err(AppEntityStoreError::StaleDatasetGeneration);
    }
    if let (Some(cursor_ref), Some(evidence_json)) = (cursor.new_cursor_ref, cursor.evidence_json) {
        let schema_revision = i64::try_from(cursor.schema_revision.get())
            .map_err(|_| AppEntityStoreError::InvalidRevision)?;
        let dataset_generation = i64::try_from(cursor.dataset_generation)
            .map_err(|_| AppEntityStoreError::CorruptCursor)?;
        let next_offset =
            i64::try_from(cursor.next_offset).map_err(|_| AppEntityStoreError::CorruptCursor)?;
        let existing = transaction
            .query_row(
                "SELECT installation_id, schema_revision, dataset_generation,
                        evidence_json, snapshot_json, snapshot_ref,
                        next_offset, expires_at
                   FROM app_query_cursors WHERE cursor_ref = ?1",
                params![cursor_ref.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing.0 != cursor.installation_id.as_str()
                || existing.1 != schema_revision
                || existing.2 != dataset_generation
                || existing.3 != evidence_json
                || existing.4 != cursor.snapshot_json
                || existing.5.as_deref() != Some(cursor.snapshot_ref.as_str())
                || existing.6 != next_offset
                || existing.7 != cursor.expires_at.to_rfc3339()
            {
                return Err(AppEntityStoreError::CorruptCursor);
            }
            transaction.commit()?;
            return Ok(());
        }

        if let Some(source_cursor_ref) = &cursor.source_cursor_ref {
            if !cursor.snapshot_json.is_empty() || source_cursor_ref == &cursor_ref {
                return Err(AppEntityStoreError::CorruptCursor);
            }
            let source = transaction
                .query_row(
                    "SELECT schema_revision, dataset_generation, snapshot_ref, expires_at
                       FROM app_query_cursors
                      WHERE cursor_ref = ?1 AND installation_id = ?2",
                    params![source_cursor_ref.as_str(), cursor.installation_id.as_str(),],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(AppEntityStoreError::MissingCursor)?;
            if source.0 != schema_revision
                || source.1 != dataset_generation
                || source.2.as_deref() != Some(cursor.snapshot_ref.as_str())
                || source.3 != cursor.expires_at.to_rfc3339()
            {
                return Err(AppEntityStoreError::CorruptCursor);
            }
            let root_exists = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM app_query_cursors
                      WHERE cursor_ref = ?1
                        AND installation_id = ?2
                        AND snapshot_ref = cursor_ref
                        AND schema_revision = ?3
                        AND dataset_generation = ?4
                        AND expires_at = ?5
                        AND length(snapshot_json) > 0
                 )",
                params![
                    cursor.snapshot_ref.as_str(),
                    cursor.installation_id.as_str(),
                    schema_revision,
                    dataset_generation,
                    cursor.expires_at.to_rfc3339(),
                ],
                |row| row.get::<_, bool>(0),
            )?;
            if !root_exists {
                return Err(AppEntityStoreError::CorruptCursor);
            }

            // Once a later cursor is consumed, older intermediate cursors no
            // longer need to retain replay authority. Keep the immutable root
            // plus the current parent so a lost response can retry exactly.
            transaction.execute(
                "DELETE FROM app_query_cursors
                  WHERE installation_id = ?1
                    AND snapshot_ref = ?2
                    AND cursor_ref <> ?2
                    AND cursor_ref <> ?3",
                params![
                    cursor.installation_id.as_str(),
                    cursor.snapshot_ref.as_str(),
                    source_cursor_ref.as_str(),
                ],
            )?;
        } else if cursor.snapshot_ref != cursor_ref || cursor.snapshot_json.is_empty() {
            return Err(AppEntityStoreError::CorruptCursor);
        }

        let (cursor_count, cursor_bytes): (i64, i64) = transaction.query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(evidence_json) + length(snapshot_json)), 0)
               FROM app_query_cursors WHERE installation_id = ?1",
            params![cursor.installation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let new_cursor_bytes = i64::try_from(
            evidence_json
                .len()
                .checked_add(cursor.snapshot_json.len())
                .ok_or(AppEntityStoreError::CursorCapacityExceeded)?,
        )
        .map_err(|_| AppEntityStoreError::CursorCapacityExceeded)?;
        if cursor_count >= MAX_QUERY_CURSORS_PER_INSTALLATION
            || cursor_bytes
                .checked_add(new_cursor_bytes)
                .map_or(true, |bytes| {
                    bytes > MAX_QUERY_CURSOR_BYTES_PER_INSTALLATION
                })
        {
            return Err(AppEntityStoreError::CursorCapacityExceeded);
        }
        transaction.execute(
            "INSERT INTO app_query_cursors (
                 cursor_ref, installation_id, schema_revision, dataset_generation,
                 evidence_json, snapshot_json, snapshot_ref, next_offset,
                 created_at, expires_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                cursor_ref.as_str(),
                cursor.installation_id.as_str(),
                schema_revision,
                dataset_generation,
                evidence_json,
                cursor.snapshot_json,
                cursor.snapshot_ref.as_str(),
                next_offset,
                now.to_rfc3339(),
                cursor.expires_at.to_rfc3339(),
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn current_dataset_generation(
    connection: &Connection,
    installation_id: &AppInstallationId,
) -> Result<u64, AppEntityStoreError> {
    let stored = connection
        .query_row(
            "SELECT current_generation FROM app_dataset_generations
             WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    match stored {
        Some(value) => {
            let value = u64::try_from(value).map_err(|_| AppEntityStoreError::CorruptDataset)?;
            if value == 0 {
                return Err(AppEntityStoreError::CorruptDataset);
            }
            Ok(value)
        },
        None => Ok(1),
    }
}

/// A mutation generation proves scalar indexes were maintained transactionally.
/// A brand-new installation without any record heads is complete too; imported
/// records without a generation must still take the legacy bounded scan.
pub fn dataset_indexes_are_complete(
    connection: &Connection,
    installation_id: &AppInstallationId,
) -> Result<bool, AppEntityStoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_dataset_generations WHERE installation_id = ?1)
             OR NOT EXISTS(SELECT 1 FROM app_record_heads WHERE installation_id = ?1 LIMIT 1)",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?)
}

fn indexed_query_filter(
    connection: &Connection,
    installation_id: &AppInstallationId,
    request: &AppQueryRequest,
    runtime: &AppEntityRuntimeContract,
) -> Result<Option<Option<super::indexed_snapshot::Filter>>, AppEntityStoreError> {
    if !dataset_indexes_are_complete(connection, installation_id)? {
        return Ok(None);
    }
    Ok(compile_indexed_query_filter(request, runtime))
}

fn compile_indexed_query_filter(
    request: &AppQueryRequest,
    runtime: &AppEntityRuntimeContract,
) -> Option<Option<super::indexed_snapshot::Filter>> {
    use super::indexed_snapshot::Filter;
    fn values(
        runtime: &AppEntityRuntimeContract,
        field: &AppFieldPath,
        values: &[Value],
    ) -> Option<Filter> {
        let contract = runtime.field(field)?;
        if !contract.indexed() || values.is_empty() {
            return None;
        }
        let values = values
            .iter()
            .map(|value| {
                let index = project_scalar_index(contract.kind(), value).ok()?;
                let value = match (index.text_value, index.integer_value) {
                    (Some(value), None) => SqlValue::Text(value),
                    (None, Some(value)) => SqlValue::Integer(value),
                    _ => return None,
                };
                Some((index.value_kind, value))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Filter::Values(field.to_string(), values))
    }
    fn filter(
        predicate: &AppPredicate,
        node: u16,
        runtime: &AppEntityRuntimeContract,
    ) -> Option<Filter> {
        match predicate.nodes.get(usize::from(node))? {
            AppPredicateNode::All { children } => Some(Filter::All(
                children
                    .iter()
                    .map(|node| filter(predicate, *node, runtime))
                    .collect::<Option<Vec<_>>>()?,
            )),
            AppPredicateNode::Any { children } => Some(Filter::Any(
                children
                    .iter()
                    .map(|node| filter(predicate, *node, runtime))
                    .collect::<Option<Vec<_>>>()?,
            )),
            AppPredicateNode::Compare {
                field,
                operator: AppComparisonOperator::Equal,
                value,
            } => values(runtime, field, std::slice::from_ref(value)),
            AppPredicateNode::In {
                field,
                values: items,
            } => values(runtime, field, items),
            _ => None,
        }
    }
    let predicate = match &request.predicate {
        Some(predicate) => match filter(predicate, predicate.root, runtime) {
            Some(filter) => Some(filter),
            None => return None,
        },
        None => None,
    };
    if predicate
        .as_ref()
        .is_some_and(|filter| filter.parameter_count() > 800)
    {
        return None;
    }
    Some(predicate)
}

fn indexed_query_snapshot(
    connection: &Connection,
    installation_id: &AppInstallationId,
    request: &AppQueryRequest,
    runtime: &AppEntityRuntimeContract,
) -> Result<Option<Vec<SnapshotEntry>>, AppEntityStoreError> {
    use super::indexed_snapshot::{self, Order};
    let Some(predicate) = indexed_query_filter(connection, installation_id, request, runtime)?
    else {
        return Ok(None);
    };
    let order = request
        .order
        .iter()
        .map(|order| Order {
            field: order.field.to_string(),
            descending: order.direction == AppOrderDirection::Descending,
        })
        .collect::<Vec<_>>();
    // Keep complex predicate arenas on the existing evaluator instead of
    // exceeding SQLite's minimum supported parameter/SQL-expression budgets.
    if predicate
        .as_ref()
        .is_some_and(|filter| filter.parameter_count() > 800)
    {
        return Ok(None);
    }
    let rows = indexed_snapshot::read(
        connection,
        installation_id.as_str(),
        request.entity.as_str(),
        predicate.as_ref(),
        &order,
        MAX_QUERY_SNAPSHOT_ROWS,
        MAX_QUERY_SCAN_BYTES,
    )
    .map_err(|error| match &error {
        rusqlite::Error::SqliteFailure(code, _)
            if code.extended_code == rusqlite::ffi::SQLITE_TOOBIG =>
        {
            AppEntityStoreError::QueryScanTooLarge
        },
        _ => error.into(),
    })?;
    if rows.len() > MAX_QUERY_SNAPSHOT_ROWS {
        return Err(AppEntityStoreError::SnapshotTooLarge);
    }
    rows.into_iter()
        .map(|(id, revision)| {
            Ok(SnapshotEntry {
                record_id: AppRecordId::parse(id)?,
                record_revision: AppRevision::new(
                    u64::try_from(revision).map_err(|_| AppEntityStoreError::InvalidRevision)?,
                )?,
            })
        })
        .collect::<Result<Vec<_>, AppEntityStoreError>>()
        .map(Some)
}

fn load_indexed_current_records(
    connection: &Connection,
    installation_id: &AppInstallationId,
    entity: &AppName,
    predicate: &AppPredicate,
    runtime: &AppEntityRuntimeContract,
) -> Result<Option<Vec<LoadedRecord>>, AppEntityStoreError> {
    let lookup = indexed_predicate_lookup(predicate, runtime)?;
    let Some(lookup) = lookup else {
        return Ok(None);
    };
    let limit = i64::try_from(MAX_QUERY_SNAPSHOT_ROWS + 1)
        .map_err(|_| AppEntityStoreError::SnapshotTooLarge)?;
    let (table, condition, field, value) = lookup;
    let sql = format!(
        "SELECT h.record_id, h.record_revision, h.dataset_generation,
                r.payload_digest, r.payload_json,
                r.handling_policy_digest, r.handling_policy_json
         FROM {table} i
         JOIN app_record_heads h
           ON h.installation_id = i.installation_id
          AND h.entity_name = i.entity_name
          AND h.record_id = i.record_id
          AND h.record_revision = i.record_revision
         JOIN app_record_revisions r
           ON r.installation_id = h.installation_id
          AND r.entity_name = h.entity_name
          AND r.record_id = h.record_id
          AND r.record_revision = h.record_revision
         WHERE i.installation_id = ?1 AND i.entity_name = ?2
           AND h.deleted_at IS NULL
           AND i.field_path = ?3 AND {condition}
         ORDER BY h.record_id ASC LIMIT ?5"
    );
    let parameters = vec![
        SqlValue::Text(installation_id.to_string()),
        SqlValue::Text(entity.to_string()),
        SqlValue::Text(field.to_string()),
        value,
        SqlValue::Integer(limit),
    ];
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Vec<u8>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Vec<u8>>(6)?,
        ))
    })?;
    let mut records = Vec::new();
    let mut decoded_bytes = 0usize;
    for row in rows {
        let (
            record_id,
            revision,
            generation,
            payload_digest,
            payload,
            handling_policy_digest,
            handling_policy,
        ) = row?;
        account_query_scan_bytes(&mut decoded_bytes, payload.len(), handling_policy.len())?;
        records.push(decode_loaded_record(
            record_id,
            revision,
            generation,
            &payload_digest,
            &payload,
            &handling_policy_digest,
            &handling_policy,
        )?);
    }
    if records.len() > MAX_QUERY_SNAPSHOT_ROWS {
        return Err(AppEntityStoreError::SnapshotTooLarge);
    }
    Ok(Some(records))
}

fn indexed_predicate_lookup(
    predicate: &AppPredicate,
    runtime: &AppEntityRuntimeContract,
) -> Result<Option<(&'static str, &'static str, AppFieldPath, SqlValue)>, AppEntityStoreError> {
    let root = predicate
        .nodes
        .get(usize::from(predicate.root))
        .ok_or(AppEntityStoreError::InvalidPredicate)?;
    let lookup = |node: &AppPredicateNode| match node {
        AppPredicateNode::Compare {
            field,
            operator,
            value,
        } => indexed_lookup(runtime, field, *operator, value),
        _ => None,
    };
    if let AppPredicateNode::All { children } = root {
        for child in children {
            let node = predicate
                .nodes
                .get(usize::from(*child))
                .ok_or(AppEntityStoreError::InvalidPredicate)?;
            if let Some(indexed) = lookup(node) {
                return Ok(Some(indexed));
            }
        }
        Ok(None)
    } else {
        Ok(lookup(root))
    }
}

fn indexed_lookup(
    runtime: &AppEntityRuntimeContract,
    field: &AppFieldPath,
    operator: AppComparisonOperator,
    value: &Value,
) -> Option<(&'static str, &'static str, AppFieldPath, SqlValue)> {
    let contract = runtime.field(field)?;
    match operator {
        AppComparisonOperator::Contains if contract.text_search() => Some((
            "app_text_search",
            "instr(i.search_text, ?4) > 0",
            field.clone(),
            SqlValue::Text(value.as_str()?.nfkc().collect()),
        )),
        AppComparisonOperator::StartsWith if contract.text_search() => Some((
            "app_text_search",
            "substr(i.search_text, 1, length(?4)) = ?4",
            field.clone(),
            SqlValue::Text(value.as_str()?.nfkc().collect()),
        )),
        AppComparisonOperator::Equal if contract.indexed() => {
            let (condition, value) = scalar_index_equality(contract.kind(), value)?;
            Some(("app_scalar_indexes", condition, field.clone(), value))
        },
        _ => None,
    }
}

fn scalar_index_equality(
    kind: AppQueryScalarKind,
    value: &Value,
) -> Option<(&'static str, SqlValue)> {
    let projected = project_scalar_index(kind, value).ok()?;
    match (
        projected.value_kind,
        projected.text_value,
        projected.integer_value,
    ) {
        ("integer", Some(value), None) => Some((
            "i.value_kind = 'integer' AND i.text_value = ?4",
            SqlValue::Text(value),
        )),
        ("integer", None, Some(value)) => Some((
            "i.value_kind = 'integer' AND i.integer_value = ?4",
            SqlValue::Integer(value),
        )),
        ("boolean", None, Some(value)) => Some((
            "i.value_kind = 'boolean' AND i.integer_value = ?4",
            SqlValue::Integer(value),
        )),
        ("decimal", Some(value), None) => Some((
            "i.value_kind = 'decimal' AND i.text_value = ?4",
            SqlValue::Text(value),
        )),
        ("timestamp", Some(value), None) => Some((
            "i.value_kind = 'timestamp' AND i.text_value = ?4",
            SqlValue::Text(value),
        )),
        ("text", Some(value), None) => Some((
            "i.value_kind = 'text' AND i.text_value = ?4",
            SqlValue::Text(value),
        )),
        ("enum", Some(value), None) => Some((
            "i.value_kind = 'enum' AND i.text_value = ?4",
            SqlValue::Text(value),
        )),
        ("reference", Some(value), None) => Some((
            "i.value_kind = 'reference' AND i.text_value = ?4",
            SqlValue::Text(value),
        )),
        _ => None,
    }
}

fn load_current_records(
    connection: &Connection,
    installation_id: &AppInstallationId,
    entity: &AppName,
) -> Result<Vec<LoadedRecord>, AppEntityStoreError> {
    let limit = i64::try_from(MAX_QUERY_SNAPSHOT_ROWS + 1)
        .map_err(|_| AppEntityStoreError::SnapshotTooLarge)?;
    let mut statement = connection.prepare(
        "SELECT h.record_id, h.record_revision, h.dataset_generation,
                r.payload_digest, r.payload_json,
                r.handling_policy_digest, r.handling_policy_json
         FROM app_record_heads h
         JOIN app_record_revisions r
           ON r.installation_id = h.installation_id
          AND r.entity_name = h.entity_name
          AND r.record_id = h.record_id
          AND r.record_revision = h.record_revision
         WHERE h.installation_id = ?1 AND h.entity_name = ?2
           AND h.deleted_at IS NULL
         ORDER BY h.record_id ASC LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![installation_id.as_str(), entity.as_str(), limit],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Vec<u8>>(6)?,
            ))
        },
    )?;
    let mut records = Vec::new();
    let mut decoded_bytes = 0usize;
    for row in rows {
        let (
            record_id,
            revision,
            generation,
            payload_digest,
            payload,
            handling_policy_digest,
            handling_policy,
        ) = row?;
        account_query_scan_bytes(&mut decoded_bytes, payload.len(), handling_policy.len())?;
        records.push(decode_loaded_record(
            record_id,
            revision,
            generation,
            &payload_digest,
            &payload,
            &handling_policy_digest,
            &handling_policy,
        )?);
    }
    if records.len() > MAX_QUERY_SNAPSHOT_ROWS {
        return Err(AppEntityStoreError::SnapshotTooLarge);
    }
    Ok(records)
}

fn load_snapshot_records(
    connection: &Connection,
    installation_id: &AppInstallationId,
    entity: &AppName,
    entries: &[SnapshotEntry],
) -> Result<Vec<LoadedRecord>, AppEntityStoreError> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let mut values = Vec::with_capacity(entries.len());
    let mut parameters = Vec::with_capacity(entries.len().saturating_mul(3).saturating_add(2));
    for (ordinal, entry) in entries.iter().enumerate() {
        values.push("(?, ?, ?)");
        parameters.push(SqlValue::Text(entry.record_id.to_string()));
        parameters.push(SqlValue::Integer(
            i64::try_from(entry.record_revision.get())
                .map_err(|_| AppEntityStoreError::InvalidRevision)?,
        ));
        parameters.push(SqlValue::Integer(
            i64::try_from(ordinal).map_err(|_| AppEntityStoreError::CorruptCursor)?,
        ));
    }
    parameters.push(SqlValue::Text(installation_id.to_string()));
    parameters.push(SqlValue::Text(entity.to_string()));
    let sql = format!(
        "WITH requested(record_id, record_revision, ordinal) AS (VALUES {})
         SELECT requested.record_id, r.record_revision, r.dataset_generation,
                r.payload_digest, r.payload_json,
                r.handling_policy_digest, r.handling_policy_json
           FROM requested
           JOIN app_record_revisions r
             ON r.record_id = requested.record_id
            AND r.record_revision = requested.record_revision
           JOIN app_record_heads h
             ON h.installation_id = r.installation_id
            AND h.entity_name = r.entity_name
            AND h.record_id = r.record_id
            AND h.record_revision = r.record_revision
          WHERE r.installation_id = ? AND r.entity_name = ?
            AND h.deleted_at IS NULL
          ORDER BY requested.ordinal ASC",
        values.join(", ")
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(parameters), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Vec<u8>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Vec<u8>>(6)?,
        ))
    })?;
    let mut loaded = Vec::with_capacity(entries.len());
    let mut decoded_bytes = 0usize;
    for row in rows {
        let (record_id, revision, generation, payload_digest, payload, policy_digest, policy) =
            row?;
        account_query_scan_bytes(&mut decoded_bytes, payload.len(), policy.len())?;
        loaded.push(decode_loaded_record(
            record_id,
            revision,
            generation,
            &payload_digest,
            &payload,
            &policy_digest,
            &policy,
        )?);
    }
    if loaded.len() != entries.len() {
        return Err(AppEntityStoreError::CursorSnapshotUnavailable);
    }
    Ok(loaded)
}

fn account_query_scan_bytes(
    total: &mut usize,
    payload_bytes: usize,
    policy_bytes: usize,
) -> Result<(), AppEntityStoreError> {
    *total = total
        .checked_add(payload_bytes)
        .and_then(|bytes| bytes.checked_add(policy_bytes))
        .ok_or(AppEntityStoreError::QueryScanTooLarge)?;
    if *total > MAX_QUERY_SCAN_BYTES {
        return Err(AppEntityStoreError::QueryScanTooLarge);
    }
    Ok(())
}

fn decode_loaded_record(
    record_id: String,
    revision: i64,
    generation: i64,
    expected_payload_digest: &str,
    payload: &[u8],
    expected_handling_policy_digest: &str,
    handling_policy: &[u8],
) -> Result<LoadedRecord, AppEntityStoreError> {
    let record_revision =
        AppRevision::new(u64::try_from(revision).map_err(|_| AppEntityStoreError::CorruptRecord)?)?;
    let dataset_generation =
        u64::try_from(generation).map_err(|_| AppEntityStoreError::CorruptRecord)?;
    let payload = decode_bounded_json_value(payload, &AppContractLimits::default())?;
    if !payload.is_object() || dataset_generation == 0 {
        return Err(AppEntityStoreError::CorruptRecord);
    }
    let expected_payload_digest = AppDigest::parse(expected_payload_digest)?;
    if AppDigest::blake3_canonical_json(&payload)? != expected_payload_digest {
        return Err(AppEntityStoreError::CorruptRecord);
    }
    let handling_value = decode_bounded_json_value(handling_policy, &AppContractLimits::default())?;
    let handling_policy: AppDataHandlingPolicy = serde_json::from_value(handling_value)?;
    validate_policy(&handling_policy, &AppContractLimits::default())?;
    let expected_policy_digest = AppDigest::parse(expected_handling_policy_digest)?;
    if AppDigest::blake3_canonical_json(&serde_json::to_value(&handling_policy)?)?
        != expected_policy_digest
    {
        return Err(AppEntityStoreError::CorruptRecord);
    }
    Ok(LoadedRecord {
        record_id: AppRecordId::parse(record_id)?,
        record_revision,
        payload,
        handling_policy,
    })
}

fn validate_loaded_records(
    runtime: &AppEntityRuntimeContract,
    records: &[LoadedRecord],
) -> Result<(), AppEntityStoreError> {
    for record in records {
        runtime.validate_payload(&record.payload)?;
    }
    Ok(())
}

fn evaluate_predicate(
    predicate: &AppPredicate,
    runtime: &AppEntityRuntimeContract,
    payload: &Value,
) -> Result<bool, AppEntityStoreError> {
    let mut results = vec![None; predicate.nodes.len()];
    let mut visiting = vec![false; predicate.nodes.len()];
    let mut stack = vec![(usize::from(predicate.root), false)];
    while let Some((index, expanded)) = stack.pop() {
        if results.get(index).and_then(|value| *value).is_some() {
            continue;
        }
        let node = predicate
            .nodes
            .get(index)
            .ok_or(AppEntityStoreError::InvalidPredicate)?;
        if expanded {
            let result = match node {
                AppPredicateNode::All { children } => children.iter().all(|child| {
                    results
                        .get(usize::from(*child))
                        .and_then(|value| *value)
                        .unwrap_or(false)
                }),
                AppPredicateNode::Any { children } => children.iter().any(|child| {
                    results
                        .get(usize::from(*child))
                        .and_then(|value| *value)
                        .unwrap_or(false)
                }),
                AppPredicateNode::Not { child } => !results
                    .get(usize::from(*child))
                    .and_then(|value| *value)
                    .ok_or(AppEntityStoreError::InvalidPredicate)?,
                AppPredicateNode::Compare {
                    field,
                    operator,
                    value,
                } => payload.get(field.as_str()).is_some_and(|stored| {
                    compare_predicate_value(runtime, field, stored, value, *operator)
                }),
                AppPredicateNode::In { field, values } => {
                    payload.get(field.as_str()).is_some_and(|stored| {
                        runtime.field(field).is_some_and(|contract| {
                            values.iter().any(|value| {
                                compare_json_values(contract.kind(), stored, value)
                                    == Some(Ordering::Equal)
                            })
                        })
                    })
                },
                AppPredicateNode::IsNull { field, negated } => match payload.get(field.as_str()) {
                    Some(value) if *negated => !value.is_null(),
                    Some(value) => value.is_null(),
                    None => false,
                },
            };
            results[index] = Some(result);
            visiting[index] = false;
            continue;
        }
        if visiting[index] {
            return Err(AppEntityStoreError::InvalidPredicate);
        }
        visiting[index] = true;
        stack.push((index, true));
        match node {
            AppPredicateNode::All { children } | AppPredicateNode::Any { children } => {
                for child in children.iter().rev() {
                    stack.push((usize::from(*child), false));
                }
            },
            AppPredicateNode::Not { child } => stack.push((usize::from(*child), false)),
            AppPredicateNode::Compare { .. }
            | AppPredicateNode::In { .. }
            | AppPredicateNode::IsNull { .. } => {},
        }
    }
    results
        .get(usize::from(predicate.root))
        .and_then(|value| *value)
        .ok_or(AppEntityStoreError::InvalidPredicate)
}

fn compare_predicate_value(
    runtime: &AppEntityRuntimeContract,
    field: &AppFieldPath,
    stored: &Value,
    expected: &Value,
    operator: AppComparisonOperator,
) -> bool {
    let Some(contract) = runtime.field(field) else {
        return false;
    };
    let ordering = compare_json_values(contract.kind(), stored, expected);
    match operator {
        AppComparisonOperator::Equal => ordering == Some(Ordering::Equal),
        AppComparisonOperator::NotEqual => ordering != Some(Ordering::Equal),
        AppComparisonOperator::LessThan => ordering == Some(Ordering::Less),
        AppComparisonOperator::LessThanOrEqual => {
            matches!(ordering, Some(Ordering::Less) | Some(Ordering::Equal))
        },
        AppComparisonOperator::GreaterThan => ordering == Some(Ordering::Greater),
        AppComparisonOperator::GreaterThanOrEqual => {
            matches!(ordering, Some(Ordering::Greater) | Some(Ordering::Equal))
        },
        AppComparisonOperator::Contains => normalized_text_pair(stored, expected)
            .is_some_and(|(stored, expected)| stored.contains(&expected)),
        AppComparisonOperator::StartsWith => normalized_text_pair(stored, expected)
            .is_some_and(|(stored, expected)| stored.starts_with(&expected)),
    }
}

fn compare_records(
    left: &LoadedRecord,
    right: &LoadedRecord,
    request: &AppQueryRequest,
    runtime: &AppEntityRuntimeContract,
) -> Ordering {
    for order in &request.order {
        let left_value = left.payload.get(order.field.as_str());
        let right_value = right.payload.get(order.field.as_str());
        let missing_or_null =
            value_presence_rank(left_value).cmp(&value_presence_rank(right_value));
        if missing_or_null != Ordering::Equal {
            return missing_or_null;
        }
        if let (Some(left_value), Some(right_value)) = (left_value, right_value) {
            let kind = runtime
                .field(&order.field)
                .map(|field| field.kind())
                .unwrap_or(AppQueryScalarKind::Text);
            if let Some(mut ordering) = compare_json_values(kind, left_value, right_value) {
                if order.direction == AppOrderDirection::Descending {
                    ordering = ordering.reverse();
                }
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
    left.record_id.cmp(&right.record_id)
}

fn value_presence_rank(value: Option<&Value>) -> u8 {
    match value {
        Some(value) if !value.is_null() => 0,
        Some(_) => 1,
        None => 2,
    }
}

fn compare_json_values(kind: AppQueryScalarKind, left: &Value, right: &Value) -> Option<Ordering> {
    match kind {
        AppQueryScalarKind::Integer => Some(integer_value(left)?.cmp(&integer_value(right)?)),
        AppQueryScalarKind::Decimal => {
            let left = Decimal::from_str(&left.to_string()).ok()?;
            let right = Decimal::from_str(&right.to_string()).ok()?;
            Some(left.cmp(&right))
        },
        AppQueryScalarKind::Boolean => left.as_bool()?.partial_cmp(&right.as_bool()?),
        AppQueryScalarKind::Timestamp => {
            let left = DateTime::parse_from_rfc3339(left.as_str()?).ok()?;
            let right = DateTime::parse_from_rfc3339(right.as_str()?).ok()?;
            Some(left.cmp(&right))
        },
        AppQueryScalarKind::Text
        | AppQueryScalarKind::Markdown
        | AppQueryScalarKind::Enum
        | AppQueryScalarKind::Reference => {
            let left = left.as_str()?.nfkc().collect::<String>();
            let right = right.as_str()?.nfkc().collect::<String>();
            Some(left.cmp(&right))
        },
    }
}

fn integer_value(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}

fn normalized_text_pair(left: &Value, right: &Value) -> Option<(String, String)> {
    Some((
        left.as_str()?.nfkc().collect(),
        right.as_str()?.nfkc().collect(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn project_record(
    connection: &Connection,
    active: &ActiveAppEntitySchema,
    request: &AppQueryRequest,
    runtime: &AppEntityRuntimeContract,
    record: LoadedRecord,
    source_refs: &mut Vec<AppSourceRef>,
    record_policies: &mut Vec<AppDataHandlingPolicy>,
    relation_rows_remaining: &mut usize,
    relation_cache: &mut BTreeMap<(AppName, AppRecordId), Option<LoadedRecord>>,
    relation_decoded_bytes: &mut usize,
) -> Result<AppRecordProjection, AppEntityStoreError> {
    let mut fields = BTreeMap::new();
    for field in &request.select {
        if let Some(value) = record.payload.get(field.as_str()) {
            fields.insert(field.clone(), value.clone());
        }
    }
    push_source_ref(
        source_refs,
        &request.entity,
        &record,
        query_policy_influence_fields(request),
    )?;
    record_policies.push(record.handling_policy.clone());
    for expansion in &request.relation_expansions {
        let expanded = expand_relation(
            connection,
            active,
            runtime,
            &record.payload,
            expansion,
            source_refs,
            record_policies,
            relation_rows_remaining,
            relation_cache,
            relation_decoded_bytes,
        )?;
        fields.insert(AppFieldPath::parse(expansion.relation.as_str())?, expanded);
    }
    Ok(AppRecordProjection {
        entity: request.entity.clone(),
        record_id: record.record_id,
        record_revision: record.record_revision,
        fields,
    })
}

fn expand_relation(
    connection: &Connection,
    active: &ActiveAppEntitySchema,
    source_runtime: &AppEntityRuntimeContract,
    payload: &Value,
    expansion: &AppRelationExpansion,
    source_refs: &mut Vec<AppSourceRef>,
    record_policies: &mut Vec<AppDataHandlingPolicy>,
    relation_rows_remaining: &mut usize,
    relation_cache: &mut BTreeMap<(AppName, AppRecordId), Option<LoadedRecord>>,
    relation_decoded_bytes: &mut usize,
) -> Result<Value, AppEntityStoreError> {
    let relation_path = AppFieldPath::parse(expansion.relation.as_str())?;
    let relation = source_runtime
        .field(&relation_path)
        .ok_or_else(|| AppEntityStoreError::UnknownRelation(expansion.relation.to_string()))?;
    let mut target_entity = relation
        .reference_entity()
        .cloned()
        .ok_or_else(|| AppEntityStoreError::UnknownRelation(expansion.relation.to_string()))?;
    if expansion.max_depth > relation.relation_max_depth().unwrap_or(0) {
        return Err(AppEntityStoreError::UnknownRelation(
            expansion.relation.to_string(),
        ));
    }
    let mut next_id = payload
        .get(expansion.relation.as_str())
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let mut expanded = Vec::new();
    let mut visited = BTreeSet::new();
    let row_limit = usize::try_from(expansion.max_rows).unwrap_or(usize::MAX);
    for depth in 0..usize::from(expansion.max_depth) {
        if expanded.len() >= row_limit {
            break;
        }
        let Some(raw_id) = next_id.take() else {
            break;
        };
        if *relation_rows_remaining == 0 {
            return Err(AppEntityStoreError::RelationProjectionTooLarge);
        }
        let record_id = AppRecordId::parse(raw_id)?;
        if !visited.insert((target_entity.clone(), record_id.clone())) {
            break;
        }
        let cache_key = (target_entity.clone(), record_id.clone());
        let record = if let Some(record) = relation_cache.get(&cache_key) {
            record.clone()
        } else {
            let record = load_current_record_by_id(
                connection,
                active.installation_id(),
                &target_entity,
                &record_id,
                relation_decoded_bytes,
            )?;
            relation_cache.insert(cache_key, record.clone());
            record
        };
        let Some(record) = record else {
            break;
        };
        let target_runtime = active
            .runtime_contract(&target_entity)
            .ok_or_else(|| AppEntityStoreError::UnknownEntity(target_entity.to_string()))?;
        target_runtime.validate_payload(&record.payload)?;
        *relation_rows_remaining = (*relation_rows_remaining).saturating_sub(1);
        record_policies.push(record.handling_policy.clone());
        let mut selected = Map::new();
        for field in &expansion.select {
            if let Some(value) = record.payload.get(field.as_str()) {
                selected.insert(field.to_string(), value.clone());
            }
        }
        expanded.push(serde_json::json!({
            "record_id": record.record_id,
            "record_revision": record.record_revision,
            "fields": selected,
        }));
        let continues = depth.saturating_add(1) < usize::from(expansion.max_depth);
        let mut source_fields = expansion.select.clone();
        if continues && target_runtime.field(&relation_path).is_some() {
            source_fields.push(relation_path.clone());
        }
        source_fields.sort();
        source_fields.dedup();
        push_source_ref(source_refs, &target_entity, &record, source_fields)?;
        next_id = if continues {
            target_runtime
                .field(&relation_path)
                .and_then(|field| field.reference_entity())
                .and_then(|next_entity| {
                    target_entity = next_entity.clone();
                    record
                        .payload
                        .get(expansion.relation.as_str())
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
        } else {
            None
        };
    }
    Ok(Value::Array(expanded))
}

fn load_current_record_by_id(
    connection: &Connection,
    installation_id: &AppInstallationId,
    entity: &AppName,
    record_id: &AppRecordId,
    decoded_bytes: &mut usize,
) -> Result<Option<LoadedRecord>, AppEntityStoreError> {
    let stored = connection
        .query_row(
            "SELECT h.record_revision, h.dataset_generation,
                    r.payload_digest, r.payload_json,
                    r.handling_policy_digest, r.handling_policy_json
             FROM app_record_heads h
             JOIN app_record_revisions r
               ON r.installation_id = h.installation_id
              AND r.entity_name = h.entity_name
              AND r.record_id = h.record_id
              AND r.record_revision = h.record_revision
             WHERE h.installation_id = ?1 AND h.entity_name = ?2
               AND h.record_id = ?3 AND h.deleted_at IS NULL",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str(),
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                ))
            },
        )
        .optional()?;
    stored
        .map(
            |(
                revision,
                generation,
                payload_digest,
                payload,
                handling_policy_digest,
                handling_policy,
            )| {
                account_query_scan_bytes(decoded_bytes, payload.len(), handling_policy.len())?;
                decode_loaded_record(
                    record_id.to_string(),
                    revision,
                    generation,
                    &payload_digest,
                    &payload,
                    &handling_policy_digest,
                    &handling_policy,
                )
            },
        )
        .transpose()
}

fn push_source_ref(
    source_refs: &mut Vec<AppSourceRef>,
    entity: &AppName,
    record: &LoadedRecord,
    fields: Vec<AppFieldPath>,
) -> Result<(), AppEntityStoreError> {
    let identity = serde_json::json!({
        "entity": entity,
        "record_id": record.record_id,
    });
    let identity = AppDigest::blake3_canonical_json(&identity)?;
    source_refs.push(AppSourceRef {
        kind: AppSourceRefKind::EntityField,
        reference: AppReference::parse(format!("record:{}", identity.as_str()))?,
        revision: Some(record.record_revision),
        fields,
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_query_page(
    scope_binding_ref: &super::models::AppScopeBindingRef,
    active: &ActiveAppEntitySchema,
    request: &AppQueryRequest,
    projections: Vec<AppRecordProjection>,
    source_refs: Vec<AppSourceRef>,
    record_policies: Vec<AppDataHandlingPolicy>,
    selected_handling_policy: AppDataHandlingPolicy,
    next_cursor: Option<AppReference>,
    read_audience: &AppStoreReadAudience,
    now: DateTime<Utc>,
) -> Result<(AppQueryPage, AppDataHandlingPolicy), AppEntityStoreError> {
    let mut unique_sources = BTreeMap::new();
    for source in source_refs {
        unique_sources.insert(
            (
                source.reference.clone(),
                source.revision,
                source.fields.clone(),
            ),
            source,
        );
    }
    if unique_sources.len() > AppContractLimits::default().max_collection_items() {
        return Err(AppEntityStoreError::SourceProjectionTooLarge);
    }
    let source_refs = unique_sources.into_values().collect::<Vec<_>>();
    let effective_policy = record_policies
        .into_iter()
        .fold(selected_handling_policy, join_handling_policy);
    ensure_read_audience_policy(read_audience, &effective_policy)?;
    let policy_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&effective_policy)?)?;
    let provenance_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(&source_refs)?)?;
    let content_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(&projections)?)?;
    let schema_material = serde_json::json!({
        "installation_id": active.installation_id(),
        "schema_revision": active.schema_revision(),
        "entity": &request.entity,
        "select": &request.select,
        "relation_expansions": &request.relation_expansions,
    });
    let schema_digest = AppDigest::blake3_canonical_json(&schema_material)?;
    let result_schema_ref =
        AppReference::parse(format!("value-schema:{}", schema_digest.as_str()))?;
    let envelope = AppDataEnvelope {
        protocol_version: AppProtocolVersion::V1,
        source: AppDataSource::AppStore,
        scope_binding_ref: scope_binding_ref.clone(),
        installation_id: active.installation_id().clone(),
        package_revision_ref: active.package_revision_ref().clone(),
        schema_revision: active.schema_revision(),
        grant_revision: active.grant_revision(),
        value_schema_ref: result_schema_ref.clone(),
        value: projections,
        source_refs,
        handling_labels: AppHandlingLabels {
            classification: effective_policy.classification_floor,
            model_processing: effective_policy.model_processing,
            policy_digest,
            provenance_digest,
        },
        content_digest,
        produced_at: now,
        expires_at: None,
    };
    let page = AppQueryPage {
        envelope,
        next_cursor,
        result_schema_ref,
    };
    page.validate_app_contract(&AppContractLimits::default())?;
    Ok((page, effective_policy))
}

fn ensure_read_audience_policy(
    read_audience: &AppStoreReadAudience,
    policy: &AppDataHandlingPolicy,
) -> Result<(), AppEntityStoreError> {
    if matches!(read_audience, AppStoreReadAudience::PersonalAgent { .. })
        && (policy.personal_agent_access != AppPersonalAgentAccess::ApprovedProjection
            || !read_audience.permits_policy(policy.classification_floor, policy.model_processing))
    {
        return Err(AppEntityStoreError::PersonalAgentPolicyDenied);
    }
    Ok(())
}

fn selected_policy(
    active: &ActiveAppEntitySchema,
    request: &AppQueryRequest,
    runtime: &AppEntityRuntimeContract,
) -> Result<AppDataHandlingPolicy, AppEntityStoreError> {
    let mut selected = Vec::new();
    for field in query_policy_influence_fields(request) {
        selected.push(
            runtime
                .field(&field)
                .ok_or_else(|| AppEntityStoreError::UnknownField(field.to_string()))?
                .effective_policy()
                .clone(),
        );
    }
    for expansion in &request.relation_expansions {
        let relation_path = AppFieldPath::parse(expansion.relation.as_str())?;
        let relation = runtime
            .field(&relation_path)
            .ok_or_else(|| AppEntityStoreError::UnknownRelation(expansion.relation.to_string()))?;
        let mut target = relation
            .reference_entity()
            .cloned()
            .ok_or_else(|| AppEntityStoreError::UnknownRelation(expansion.relation.to_string()))?;
        for depth in 0..usize::from(expansion.max_depth) {
            let target_runtime = active
                .runtime_contract(&target)
                .ok_or_else(|| AppEntityStoreError::UnknownEntity(target.to_string()))?;
            for field in &expansion.select {
                selected.push(
                    target_runtime
                        .field(field)
                        .ok_or_else(|| AppEntityStoreError::UnknownField(field.to_string()))?
                        .effective_policy()
                        .clone(),
                );
            }
            if depth.saturating_add(1) >= usize::from(expansion.max_depth) {
                break;
            }
            let Some(next_relation) = target_runtime.field(&relation_path) else {
                break;
            };
            selected.push(next_relation.effective_policy().clone());
            let Some(next_target) = next_relation.reference_entity() else {
                break;
            };
            target = next_target.clone();
        }
    }
    selected
        .into_iter()
        .reduce(join_handling_policy)
        .ok_or(AppEntityStoreError::EmptyProjection)
}

/// Canonical root-record fields whose values or policies influence a query
/// result. Composition and memory proposal receipts use this exact helper so
/// predicate/order taint cannot be lost when only projected values cross the
/// next boundary. Relation expansion provenance is separately unsupported by
/// those V1 adapters.
pub(crate) fn query_policy_influence_fields(request: &AppQueryRequest) -> Vec<AppFieldPath> {
    let mut fields = request.select.clone();
    fields.extend(request.order.iter().map(|order| order.field.clone()));
    fields.extend(
        request
            .relation_expansions
            .iter()
            .filter_map(|expansion| AppFieldPath::parse(expansion.relation.as_str()).ok()),
    );
    if let Some(predicate) = &request.predicate {
        let mut pending = vec![usize::from(predicate.root)];
        let mut visited = BTreeSet::new();
        while let Some(index) = pending.pop() {
            if !visited.insert(index) {
                continue;
            }
            let Some(node) = predicate.nodes.get(index) else {
                continue;
            };
            match node {
                AppPredicateNode::All { children } | AppPredicateNode::Any { children } => {
                    pending.extend(children.iter().map(|child| usize::from(*child)));
                },
                AppPredicateNode::Not { child } => pending.push(usize::from(*child)),
                AppPredicateNode::Compare { field, .. }
                | AppPredicateNode::In { field, .. }
                | AppPredicateNode::IsNull { field, .. } => fields.push(field.clone()),
            }
        }
    }
    fields.sort();
    fields.dedup();
    fields
}

fn join_handling_policy(
    left: AppDataHandlingPolicy,
    right: AppDataHandlingPolicy,
) -> AppDataHandlingPolicy {
    super::policy::intersect_app_data_handling_policies(&left, &right)
}

fn encode_snapshot(snapshot: &[SnapshotEntry]) -> Result<Vec<u8>, AppEntityStoreError> {
    if snapshot.is_empty() || snapshot.len() > MAX_QUERY_SNAPSHOT_ROWS {
        return Err(AppEntityStoreError::SnapshotTooLarge);
    }
    let count = u32::try_from(snapshot.len()).map_err(|_| AppEntityStoreError::SnapshotTooLarge)?;
    let mut bytes = Vec::with_capacity(
        QUERY_SNAPSHOT_MAGIC
            .len()
            .saturating_add(4)
            .saturating_add(snapshot.len().saturating_mul(24)),
    );
    bytes.extend_from_slice(QUERY_SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&count.to_be_bytes());
    for entry in snapshot {
        let record_id = entry.record_id.as_str().as_bytes();
        let record_id_len =
            u16::try_from(record_id.len()).map_err(|_| AppEntityStoreError::SnapshotTooLarge)?;
        bytes.extend_from_slice(&record_id_len.to_be_bytes());
        bytes.extend_from_slice(record_id);
        bytes.extend_from_slice(&entry.record_revision.get().to_be_bytes());
        if bytes.len() > MAX_QUERY_SNAPSHOT_BYTES {
            return Err(AppEntityStoreError::SnapshotTooLarge);
        }
    }
    Ok(bytes)
}

fn decode_snapshot(bytes: &[u8]) -> Result<Vec<SnapshotEntry>, AppEntityStoreError> {
    if bytes.len() > MAX_QUERY_SNAPSHOT_BYTES || !bytes.starts_with(QUERY_SNAPSHOT_MAGIC) {
        return Err(AppEntityStoreError::CorruptCursor);
    }
    let mut offset = QUERY_SNAPSHOT_MAGIC.len();
    let count = read_snapshot_u32(bytes, &mut offset)? as usize;
    if count == 0 || count > MAX_QUERY_SNAPSHOT_ROWS {
        return Err(AppEntityStoreError::CorruptCursor);
    }
    let mut snapshot = Vec::with_capacity(count);
    let mut unique = BTreeSet::new();
    for _ in 0..count {
        let id_len = usize::from(read_snapshot_u16(bytes, &mut offset)?);
        let id_end = offset
            .checked_add(id_len)
            .filter(|end| *end <= bytes.len())
            .ok_or(AppEntityStoreError::CorruptCursor)?;
        let id = std::str::from_utf8(&bytes[offset..id_end])
            .map_err(|_| AppEntityStoreError::CorruptCursor)?;
        offset = id_end;
        let record_id =
            AppRecordId::parse(id.to_owned()).map_err(|_| AppEntityStoreError::CorruptCursor)?;
        if !unique.insert(record_id.clone()) {
            return Err(AppEntityStoreError::CorruptCursor);
        }
        let revision = AppRevision::new(read_snapshot_u64(bytes, &mut offset)?)
            .map_err(|_| AppEntityStoreError::CorruptCursor)?;
        snapshot.push(SnapshotEntry {
            record_id,
            record_revision: revision,
        });
    }
    if offset != bytes.len() {
        return Err(AppEntityStoreError::CorruptCursor);
    }
    Ok(snapshot)
}

fn read_snapshot_u16(bytes: &[u8], offset: &mut usize) -> Result<u16, AppEntityStoreError> {
    let raw = read_snapshot_bytes::<2>(bytes, offset)?;
    Ok(u16::from_be_bytes(raw))
}

fn read_snapshot_u32(bytes: &[u8], offset: &mut usize) -> Result<u32, AppEntityStoreError> {
    let raw = read_snapshot_bytes::<4>(bytes, offset)?;
    Ok(u32::from_be_bytes(raw))
}

fn read_snapshot_u64(bytes: &[u8], offset: &mut usize) -> Result<u64, AppEntityStoreError> {
    let raw = read_snapshot_bytes::<8>(bytes, offset)?;
    Ok(u64::from_be_bytes(raw))
}

fn read_snapshot_bytes<const N: usize>(
    bytes: &[u8],
    offset: &mut usize,
) -> Result<[u8; N], AppEntityStoreError> {
    let end = offset
        .checked_add(N)
        .filter(|end| *end <= bytes.len())
        .ok_or(AppEntityStoreError::CorruptCursor)?;
    let raw = bytes[*offset..end]
        .try_into()
        .map_err(|_| AppEntityStoreError::CorruptCursor)?;
    *offset = end;
    Ok(raw)
}

pub fn resolve_active_schema(
    connection: &Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
) -> Result<Option<ActiveAppEntitySchema>, AppEntityStoreError> {
    resolve_entity_schema(connection, scope, installation_id, true)
}

/// Resolve one manifest-reviewed behavior input from its exact package-owned
/// singleton record. This is an internal scheduler boundary, not a generic
/// query adapter: the selector is already package/grant digest-bound, there is
/// no predicate/first-row fallback, and the selected values plus exact record
/// revision are sealed into the returned AppStore envelope in this SQLite
/// snapshot.
pub(crate) struct AppBehaviorInputMaterial {
    pub(crate) envelope: AppDataEnvelope<Value>,
    pub(crate) handling_policy: AppDataHandlingPolicy,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_behavior_input_envelope_in_snapshot(
    connection: &Connection,
    scope: &AppScope,
    scope_binding_ref: &AppScopeBindingRef,
    installation_id: &AppInstallationId,
    installation_generation: u64,
    package_revision_ref: &AppReference,
    schema_revision: AppRevision,
    grant_revision: AppRevision,
    selector: &AppManifestBehaviorInputSelector,
    value_schema_ref: AppReference,
    now: DateTime<Utc>,
) -> Result<AppBehaviorInputMaterial, AppEntityStoreError> {
    let active = resolve_active_schema(connection, scope, installation_id)?
        .ok_or(AppEntityStoreError::MissingInstallation)?;
    if active.installation_generation() != installation_generation
        || active.package_revision_ref() != package_revision_ref
        || active.schema_revision() != schema_revision
        || active.grant_revision() != grant_revision
    {
        return Err(AppEntityStoreError::StaleGrantBinding);
    }
    let runtime = active
        .runtime_contract(&selector.entity)
        .ok_or_else(|| AppEntityStoreError::UnknownEntity(selector.entity.to_string()))?;
    let select = selector
        .fields
        .iter()
        .map(|field| AppFieldPath::parse(field.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let request = AppQueryRequest {
        pagination: Default::default(),
        protocol_version: AppProtocolVersion::V1,
        source_installation_id: installation_id.clone(),
        entity: selector.entity.clone(),
        select: select.clone(),
        predicate: None,
        order: Vec::new(),
        cursor: None,
        limit: 1,
        relation_expansions: Vec::new(),
        purpose: AppName::parse("background_behavior")?,
    };
    validate_typed_query(&request, runtime.query_schema())?;
    let selected_handling_policy = selected_policy(&active, &request, runtime)?;
    let mut decoded_bytes = 0usize;
    let record = load_current_record_by_id(
        connection,
        installation_id,
        &selector.entity,
        &selector.record_id,
        &mut decoded_bytes,
    )?
    .ok_or(AppEntityStoreError::MissingBehaviorSourceRecord)?;
    runtime.validate_payload(&record.payload)?;

    let payload = record
        .payload
        .as_object()
        .ok_or(AppEntityStoreError::CorruptRecord)?;
    let mut selected = Map::new();
    for field in &selector.fields {
        let value = payload
            .get(field.as_str())
            .ok_or_else(|| AppEntityStoreError::UnknownField(field.to_string()))?;
        selected.insert(field.to_string(), value.clone());
    }
    let value = Value::Object(selected);
    let effective_policy =
        join_handling_policy(selected_handling_policy, record.handling_policy.clone());
    ensure_read_audience_policy(&AppStoreReadAudience::AppRuntime, &effective_policy)?;
    let policy_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&effective_policy)?)?;
    let mut source_refs = Vec::with_capacity(1);
    push_source_ref(&mut source_refs, &selector.entity, &record, select)?;
    let content_digest = AppDigest::blake3_canonical_json(&value)?;
    let provenance_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "source_refs": &source_refs,
        "content_digest": &content_digest,
        "selector": selector,
    }))?;
    let envelope = AppDataEnvelope {
        protocol_version: AppProtocolVersion::V1,
        source: AppDataSource::AppStore,
        scope_binding_ref: scope_binding_ref.clone(),
        installation_id: installation_id.clone(),
        package_revision_ref: package_revision_ref.clone(),
        schema_revision,
        grant_revision,
        value_schema_ref,
        value,
        source_refs,
        handling_labels: AppHandlingLabels {
            classification: effective_policy.classification_floor,
            model_processing: effective_policy.model_processing,
            policy_digest,
            provenance_digest,
        },
        content_digest,
        produced_at: now,
        expires_at: None,
    };
    envelope.validate_app_contract(&AppContractLimits::default())?;
    Ok(AppBehaviorInputMaterial {
        envelope,
        handling_policy: effective_policy,
    })
}

/// Typed timestamp boundary shared by indexed reads and app-data maintenance.
pub fn timestamp_order_key(before: DateTime<Utc>) -> Result<Vec<u8>, AppEntityStoreError> {
    Ok(super::indexed_snapshot::order_key(
        "timestamp",
        Some(&before.to_rfc3339()),
        None,
        false,
    )?)
}

/// Resolve schema/data ownership for user-directed export, forget and
/// retention. Revocation or a non-enabled app blocks execution authority, not
/// the owner's ability to retrieve or erase personal data. Purged installs
/// remain unavailable.
pub fn resolve_data_owner_schema(
    connection: &Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
) -> Result<Option<ActiveAppEntitySchema>, AppEntityStoreError> {
    resolve_entity_schema(connection, scope, installation_id, false)
}

fn resolve_entity_schema(
    connection: &Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    require_enabled: bool,
) -> Result<Option<ActiveAppEntitySchema>, AppEntityStoreError> {
    let installation_bytes = connection
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    let Some(installation_bytes) = installation_bytes else {
        return Ok(None);
    };
    let installation: AppInstallation = decode_contract(&installation_bytes)?;
    if &installation.scope != scope || &installation.installation_id != installation_id {
        return Err(AppEntityStoreError::ScopeOrIdentityMismatch);
    }
    if (require_enabled && installation.lifecycle.status != AppInstallationStatus::Enabled)
        || installation.lifecycle.status == AppInstallationStatus::Purged
    {
        return Err(AppEntityStoreError::InstallationNotEnabled(
            installation.lifecycle.status,
        ));
    }
    let active_revision = installation
        .active_schema_revision
        .ok_or(AppEntityStoreError::MissingActiveSchemaRevision)?;
    let grant_revision = installation
        .grant_revision
        .ok_or(AppEntityStoreError::MissingGrantRevision)?;
    let grant_revision_i64 =
        i64::try_from(grant_revision.get()).map_err(|_| AppEntityStoreError::InvalidRevision)?;
    let grant_bytes = connection
        .query_row(
            "SELECT record_json FROM app_grant_revisions
             WHERE installation_id = ?1 AND revision = ?2",
            params![installation_id.as_str(), grant_revision_i64],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(AppEntityStoreError::MissingGrantRecord)?;
    let grant: AppGrantRevision = decode_contract(&grant_bytes)?;
    let granted_policy_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(
        &grant.granted_data_handling_policy,
    )?)?;
    if grant.installation_id != *installation_id
        || grant.revision != grant_revision
        || grant.package_revision_ref != installation.package_revision_ref
        || (require_enabled && grant.revoked_at.is_some())
        || grant.granted_data_handling_policy_digest != granted_policy_digest
    {
        return Err(AppEntityStoreError::StaleGrantBinding);
    }
    let revision_i64 =
        i64::try_from(active_revision.get()).map_err(|_| AppEntityStoreError::InvalidRevision)?;
    let schema_bytes = connection
        .query_row(
            "SELECT record_json FROM app_schema_revisions
             WHERE installation_id = ?1 AND revision = ?2",
            params![installation_id.as_str(), revision_i64],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(AppEntityStoreError::MissingActiveSchemaRecord)?;
    let schema: AppSchemaRevision = decode_contract(&schema_bytes)?;
    if schema.installation_id != *installation_id
        || schema.revision != active_revision
        || schema.package_revision_ref != installation.package_revision_ref
    {
        return Err(AppEntityStoreError::StaleSchemaBinding);
    }
    if schema.canonical_data_handling_policy != grant.granted_data_handling_policy {
        return Err(AppEntityStoreError::StaleGrantBinding);
    }

    let package_bytes = connection
        .query_row(
            "SELECT record_json FROM app_package_revisions
             WHERE package_revision_ref = ?1",
            params![installation.package_revision_ref.as_str()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(AppEntityStoreError::MissingPackageRevision)?;
    let package: AppPackageRevision = decode_contract(&package_bytes)?;
    if source_entity_schema_digest_from_revision(&schema)? != package.entity_schema_digest {
        return Err(AppEntityStoreError::PackageSchemaDigestMismatch);
    }
    let runtime_contracts = runtime_contracts_from_revision(&schema)?;

    Ok(Some(ActiveAppEntitySchema {
        installation_id: installation.installation_id,
        installation_generation: installation.lifecycle.generation,
        package_revision_ref: installation.package_revision_ref,
        active_surface_revision: installation.active_surface_revision,
        grant,
        schema,
        runtime_contracts,
    }))
}

fn decode_contract<T>(bytes: &[u8]) -> Result<T, AppEntityStoreError>
where
    T: DeserializeOwned + ValidateAppContract,
{
    let value = decode_bounded_json_value(bytes, &AppContractLimits::default())?;
    let value: T = serde_json::from_value(value)?;
    value.validate_app_contract(&AppContractLimits::default())?;
    Ok(value)
}

#[derive(Debug, Error)]
pub enum AppEntityStoreError {
    #[error("app entity store is unavailable: {0}")]
    Registry(#[from] AppRegistryError),
    #[error("app entity store SQLite read failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("app entity store record is malformed: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("app entity store record violates its contract: {0}")]
    Contract(#[from] AppContractError),
    #[error("app entity schema compilation failed: {0}")]
    Schema(#[from] AppSchemaCompilerError),
    #[error("app-store authority check failed: {0}")]
    Boundary(#[from] AppBoundaryError),
    #[error("app query contract failed: {0}")]
    Query(#[from] AppQuerySemanticsError),
    #[error("app installation is not enabled ({0:?})")]
    InstallationNotEnabled(AppInstallationStatus),
    #[error("app installation has no active schema revision")]
    MissingActiveSchemaRevision,
    #[error("app installation has no active grant revision")]
    MissingGrantRevision,
    #[error("active app grant revision does not exist")]
    MissingGrantRecord,
    #[error("active app schema revision does not exist")]
    MissingActiveSchemaRecord,
    #[error("active app package revision does not exist")]
    MissingPackageRevision,
    #[error("active app schema is stale or bound to another package")]
    StaleSchemaBinding,
    #[error("active app grant is revoked, corrupt or bound to another package/schema")]
    StaleGrantBinding,
    #[error("active app schema does not match its immutable package digest")]
    PackageSchemaDigestMismatch,
    #[error("app entity store scope or installation identity does not match")]
    ScopeOrIdentityMismatch,
    #[error("app schema revision exceeds the SQLite integer range")]
    InvalidRevision,
    #[error("scoped app storage does not exist")]
    MissingScopedStore,
    #[error("app installation does not exist")]
    MissingInstallation,
    #[error("the reviewed app behavior source record does not exist")]
    MissingBehaviorSourceRecord,
    #[error("app query targets unknown entity `{0}`")]
    UnknownEntity(String),
    #[error("app query targets unknown field `{0}`")]
    UnknownField(String),
    #[error("app query targets unknown relation `{0}`")]
    UnknownRelation(String),
    #[error("app query predicate is structurally invalid")]
    InvalidPredicate,
    #[error("keyset pagination requires complete indexes, indexed equality/in predicates, and at most one indexed order field") ]
    KeysetIndexRequired,
    #[error("app query snapshot exceeds the bounded retained-row ceiling")]
    SnapshotTooLarge,
    #[error("app query scan exceeds the bounded decoded-byte ceiling")]
    QueryScanTooLarge,
    #[error("app query cursor does not exist")]
    MissingCursor,
    #[error("app query cursor storage is corrupt")]
    CorruptCursor,
    #[error("app query cursor storage has reached its bounded installation quota")]
    CursorCapacityExceeded,
    #[error("app query cursor references a revision no longer retained")]
    CursorSnapshotUnavailable,
    #[error("app dataset generation is corrupt")]
    CorruptDataset,
    #[error("app dataset head predates the retained cursor snapshot")]
    StaleDatasetGeneration,
    #[error("app record storage is corrupt")]
    CorruptRecord,
    #[error("app query source projection exceeds the canonical envelope limit")]
    SourceProjectionTooLarge,
    #[error("app query relation projection exceeds the canonical envelope limit")]
    RelationProjectionTooLarge,
    #[error("app query has no selected handling policy")]
    EmptyProjection,
    #[error("app query projection is not eligible for this personal-agent audience/provider")]
    PersonalAgentPolicyDenied,
    #[error("app record projection is malformed or does not match its query-page provenance")]
    InvalidRecordProjection,
    #[error("app record projection is no longer current")]
    StaleRecordProjection,
    #[error("sealed Recipe record locator was substituted or is malformed")]
    RecipeRecordLocatorSubstitution,
    #[error("app query cursor expiry is not representable")]
    InvalidCursorExpiry,
    #[error("app entity index projection failed")]
    Index,
}

impl From<AppEntityIndexError> for AppEntityStoreError {
    fn from(_: AppEntityIndexError) -> Self {
        Self::Index
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::magician_v2::{
        apps::{
            boundary::AppStoreAuthorityFence,
            lifecycle::AppInstallationLifecycle,
            manifest::{canonical_view_schema_digest, parse_app_manifest_yaml},
            models::{
                AppDataClassification, AppDigest, AppModelProcessing, AppOrderDirection,
                AppQueryOrder,
            },
            records::{
                AppBackgroundExecution, AppCompatibilityRequirement, AppDataHandlingPolicy,
                AppEntityProjectionGrant, AppExternalEgress, AppGrantRevision, AppMemoryPromotion,
                AppNetworkPolicy, AppPackageSourceKind, AppPersonalAgentAccess, AppResourceCeiling,
            },
            registry::tests::{authenticated_scope, canonical_tempdir, time},
            schema_compiler::{canonical_entity_schema_digest, compile_app_schema},
            surface_compiler::{compile_app_surface_set, VerifiedAppSurfaceSource},
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    pub fn manifest() -> super::super::manifest::AppPackageManifest {
        canonical_manifest().manifest().clone()
    }

    pub fn canonical_manifest() -> super::super::manifest::CanonicalAppManifest {
        parse_app_manifest_yaml(
            br#"
name: reading-list
version: 0.1.0
description: A private reading list.
metadata:
  magician:
    skill_type: app
    app_manifest_version: "1.0"
    app_sdk_version: "1"
app:
  compatibility: { magician_contract: "1" }
  data_policy:
    defaults:
      classification_floor: personal
      model_processing: local_only
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
    item:
      fields:
        title: { type: text, required: true }
        status: { type: enum, values: [open, done], required: true }
        parent: { type: reference, entity: item, nullable: true, cycle_policy: allow_bounded, max_traversal_depth: 2 }
    note:
      fields:
        body: { type: text, required: true }
        item: { type: reference, entity: item, nullable: true, on_delete: nullify }
    branch:
      fields:
        label: { type: text, required: true }
        item: { type: reference, entity: item, nullable: true, on_delete: cascade }
    node:
      fields:
        label: { type: text, required: true }
        next: { type: reference, entity: node, nullable: true }
  views:
    items:
      entity: item
      kind: table
      route: /
      columns: [title, status]
  workflows: {}
  actions: {}
  resources:
    per_run: { max_tokens: 1000, max_cost_usd: 0.25, max_active_seconds: 60 }
    monthly: { max_tokens: 10000, max_cost_usd: 5.00 }
    storage: { max_records: 10000, max_bytes: 10485760 }
  dependencies: { procedure_skills: [], capabilities: [] }
  assets: []
"#,
            &super::super::manifest::AppPackageLimits::default(),
        )
        .unwrap()
    }

    pub fn policy() -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    pub fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    pub fn resources() -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 1_000,
            max_output_tokens: 1_000,
            max_cost_microusd: 100_000,
            max_paid_tool_invocations: 10,
            max_active_seconds: 60,
            max_lifetime_seconds: 600,
            max_browser_network_actions: 10,
            max_concurrent_foreground_runs: 1,
            max_concurrent_background_runs: 0,
            max_records: 10_000,
            max_payload_bytes: 10_485_760,
            max_attachment_bytes: 10_485_760,
            max_monthly_tokens: 10_000,
            max_monthly_cost_microusd: 5_000_000,
        }
    }

    fn recipe_locator_page(scope_binding_ref: &str, record_revision: u64) -> AppQueryPage {
        let projection = AppRecordProjection {
            entity: AppName::parse("item").unwrap(),
            record_id: AppRecordId::parse("record_private_1").unwrap(),
            record_revision: AppRevision::new(record_revision).unwrap(),
            fields: BTreeMap::from([(
                AppFieldPath::parse("title").unwrap(),
                Value::String("sealed title".to_owned()),
            )]),
        };
        let source_refs = vec![AppSourceRef {
            kind: AppSourceRefKind::EntityRecord,
            reference: AppReference::parse("entity-record:item:record_private_1").unwrap(),
            revision: Some(projection.record_revision),
            fields: vec![AppFieldPath::parse("title").unwrap()],
        }];
        let handling_labels = AppHandlingLabels {
            classification: AppDataClassification::Personal,
            model_processing: AppModelProcessing::LocalOnly,
            policy_digest: AppDigest::blake3(b"recipe-locator-policy"),
            provenance_digest: AppDigest::blake3_canonical_json(
                &serde_json::to_value(&source_refs).unwrap(),
            )
            .unwrap(),
        };
        let value = vec![projection];
        let result_schema_ref = AppReference::parse("value-schema:recipe-locator").unwrap();
        AppQueryPage {
            envelope: AppDataEnvelope {
                protocol_version: AppProtocolVersion::V1,
                source: AppDataSource::AppStore,
                scope_binding_ref: AppScopeBindingRef::parse(scope_binding_ref).unwrap(),
                installation_id: AppInstallationId::parse("install_recipe_locator").unwrap(),
                package_revision_ref: AppReference::parse("package-revision:recipe-locator")
                    .unwrap(),
                schema_revision: AppRevision::new(7).unwrap(),
                grant_revision: AppRevision::new(9).unwrap(),
                value_schema_ref: result_schema_ref.clone(),
                content_digest: AppDigest::blake3_canonical_json(
                    &serde_json::to_value(&value).unwrap(),
                )
                .unwrap(),
                value,
                source_refs,
                handling_labels,
                produced_at: time(1),
                expires_at: None,
            },
            next_cursor: None,
            result_schema_ref,
        }
    }

    #[test]
    fn recipe_locator_rejects_cross_scope_and_stale_revision_replays() {
        let page = recipe_locator_page("scope_anonymous_default", 3);
        let projection = page.envelope.value.first().unwrap();
        let projection_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(projection).unwrap()).unwrap();
        let locator = AppRecipeRecordLocator::seal(
            AppReference::parse(format!(
                "entity:{}",
                projection_digest.as_str().trim_start_matches("blake3:")
            ))
            .unwrap(),
            AppReference::parse("workflow-schema:recipe-locator").unwrap(),
            BTreeMap::from([
                ("entity".to_owned(), Value::String("item".to_owned())),
                ("limit".to_owned(), Value::from(1)),
                ("select".to_owned(), serde_json::json!(["title"])),
            ]),
            &page,
            projection,
            projection_digest,
        )
        .unwrap();

        locator.verify_replayed_page(&page).unwrap();

        let mut cross_scope = page.clone();
        cross_scope.envelope.scope_binding_ref =
            AppScopeBindingRef::parse("scope_other_default").unwrap();
        assert!(matches!(
            locator.verify_replayed_page(&cross_scope),
            Err(AppEntityStoreError::StaleRecordProjection)
        ));

        let mut stale_revision = page;
        stale_revision.envelope.value[0].record_revision = AppRevision::new(4).unwrap();
        stale_revision.envelope.content_digest = AppDigest::blake3_canonical_json(
            &serde_json::to_value(&stale_revision.envelope.value).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            locator.verify_replayed_page(&stale_revision),
            Err(AppEntityStoreError::StaleRecordProjection)
        ));
    }

    fn fixture_package(
        entity_schema_digest: AppDigest,
        view_schema_digest: AppDigest,
    ) -> AppPackageRevision {
        AppPackageRevision {
            package_id: reference("app:reading-list"),
            semantic_version: "0.1.0".to_owned(),
            content_digest: AppDigest::blake3(b"package"),
            manifest_schema_version: "1.0".to_owned(),
            authoring_sdk_version: "1".to_owned(),
            publisher_identity: reference("publisher:local-owner"),
            source_kind: AppPackageSourceKind::LocalVibedev,
            compatibility: vec![AppCompatibilityRequirement {
                contract: AppName::parse("magician_contract").unwrap(),
                requirement: "1".to_owned(),
            }],
            requested_authority_digest: AppDigest::blake3(b"authority"),
            requested_data_policy_digest: AppDigest::blake3(b"policy"),
            dependency_lock_digest: AppDigest::blake3(b"lock"),
            entity_schema_digest,
            view_schema_digest,
            workflow_digest: AppDigest::blake3(b"workflows"),
            verification_attestation_ref: None,
            conformance_attestation_ref: reference("attestation:conformance"),
            created_at: time(1),
        }
    }

    pub fn package_revision_ref() -> AppReference {
        let canonical_manifest = canonical_manifest();
        let entity_schema_digest =
            canonical_entity_schema_digest(canonical_manifest.manifest()).unwrap();
        let view_schema_digest =
            canonical_view_schema_digest(canonical_manifest.manifest()).unwrap();
        super::super::registry::canonical_package_revision_ref(&fixture_package(
            entity_schema_digest,
            view_schema_digest,
        ))
        .unwrap()
    }

    pub async fn seed_enabled_installation(
        registry: &AppRegistryService,
        package_schema_digest: AppDigest,
        compiled: AppSchemaRevision,
        status: AppInstallationStatus,
    ) -> AppReference {
        let scope = authenticated_scope("anonymous", "default");
        let mut compiled = compiled;
        let canonical_manifest = canonical_manifest();
        let view_schema_digest =
            canonical_view_schema_digest(canonical_manifest.manifest()).unwrap();
        let package = fixture_package(package_schema_digest.clone(), view_schema_digest.clone());
        let package_ref = super::super::registry::canonical_package_revision_ref(&package).unwrap();
        compiled.package_revision_ref = package_ref.clone();
        let installation = AppInstallation {
            scope: scope.scope().clone(),
            installation_id: compiled.installation_id.clone(),
            package_revision_ref: package_ref.clone(),
            lifecycle: AppInstallationLifecycle {
                status,
                generation: 2,
                update_return_status: None,
            },
            grant_revision: Some(AppRevision::new(1).unwrap()),
            active_schema_revision: Some(compiled.revision),
            active_surface_revision: Some(AppRevision::new(1).unwrap()),
            created_at: time(1),
            updated_at: time(2),
            disabled_at: (status == AppInstallationStatus::Disabled).then(|| time(2)),
            quarantined_at: (status == AppInstallationStatus::Quarantined).then(|| time(2)),
            uninstalled_at: (status == AppInstallationStatus::UninstalledRetained).then(|| time(2)),
            purged_at: (status == AppInstallationStatus::Purged).then(|| time(2)),
        };
        let policy = policy();
        let policy_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&policy).unwrap()).unwrap();
        let personal_agent_projection = AppEntityProjectionGrant {
            entity: AppName::parse("item").unwrap(),
            fields: vec![
                AppFieldPath::parse("title").unwrap(),
                AppFieldPath::parse("status").unwrap(),
                AppFieldPath::parse("parent").unwrap(),
            ],
            search: true,
        };
        let grant = AppGrantRevision {
            installation_id: installation.installation_id.clone(),
            revision: AppRevision::new(1).unwrap(),
            package_revision_ref: package_ref.clone(),
            requested_tools: Vec::new(),
            granted_tools: Vec::new(),
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
            requested_context_reads: Vec::new(),
            granted_context_reads: Vec::new(),
            requested_personal_agent_data_access: vec![personal_agent_projection.clone()],
            granted_personal_agent_data_access: vec![personal_agent_projection],
            requested_data_handling_policy: policy.clone(),
            granted_data_handling_policy: policy,
            granted_data_handling_policy_digest: policy_digest,
            requested_background_execution: AppBackgroundExecution::Denied,
            granted_background_execution: AppBackgroundExecution::Denied,
            requested_network_policy: AppNetworkPolicy::Denied,
            granted_network_policy: AppNetworkPolicy::Denied,
            requested_resource_ceiling: resources(),
            granted_resource_ceiling: resources(),
            approved_by: reference("actor:owner"),
            approved_at: time(1),
            authority_digest: AppDigest::blake3(b"authority"),
            revoked_at: None,
        };
        let package_json = serde_json::to_vec(&package).unwrap();
        let installation_json = super::super::registry::encode_bounded_json(
            &installation,
            &AppContractLimits::default(),
        )
        .unwrap();
        let lifecycle_status = serde_json::to_value(status)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        let grant_json = serde_json::to_vec(&grant).unwrap();
        let schema_json = serde_json::to_vec(&compiled).unwrap();
        let surface_entity_schema_digest =
            canonical_entity_schema_digest(canonical_manifest.manifest()).unwrap();
        let surface_set = compile_app_surface_set(
            &VerifiedAppSurfaceSource::for_test(
                canonical_manifest,
                installation.installation_id.clone(),
                package_ref.clone(),
                compiled.revision,
                compiled.created_at,
                surface_entity_schema_digest,
                view_schema_digest,
            ),
            AppRevision::new(1).unwrap(),
        )
        .unwrap();
        let seeded_package_ref = package_ref.clone();
        registry
            .execute_scoped_write(&scope, &time(3), move |connection, _| {
                let transaction = connection.transaction()?;
                transaction.execute(
                    "INSERT INTO app_package_revisions (
                         package_revision_ref, package_id, semantic_version,
                         content_digest, publisher_identity, dependency_lock_digest,
                         record_json, dependency_lock_json, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        package_ref.as_str(),
                        package.package_id.as_str(),
                        package.semantic_version,
                        package.content_digest.as_str(),
                        package.publisher_identity.as_str(),
                        package.dependency_lock_digest.as_str(),
                        package_json,
                        b"{}",
                        package.created_at.to_rfc3339(),
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO app_installations (
                         installation_id, principal, workspace, package_revision_ref,
                         lifecycle_status, lifecycle_generation, record_json,
                         created_at, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        installation.installation_id.as_str(),
                        installation.scope.principal.as_str(),
                        installation.scope.workspace.as_str(),
                        installation.package_revision_ref.as_str(),
                        lifecycle_status,
                        2_i64,
                        installation_json,
                        installation.created_at.to_rfc3339(),
                        installation.updated_at.to_rfc3339(),
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO app_grant_revisions (
                         installation_id, revision, package_revision_ref,
                         authority_digest, granted_data_policy_digest, revoked_at,
                         record_json, created_at
                     ) VALUES (?1, 1, ?2, ?3, ?4, NULL, ?5, ?6)",
                    params![
                        grant.installation_id.as_str(),
                        grant.package_revision_ref.as_str(),
                        grant.authority_digest.as_str(),
                        grant.granted_data_handling_policy_digest.as_str(),
                        grant_json,
                        grant.approved_at.to_rfc3339(),
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO app_schema_revisions (
                         installation_id, revision, package_revision_ref, record_json, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        compiled.installation_id.as_str(),
                        i64::try_from(compiled.revision.get()).unwrap(),
                        compiled.package_revision_ref.as_str(),
                        schema_json,
                        compiled.created_at.to_rfc3339(),
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO app_surface_generations (
                         installation_id, revision, package_revision_ref, schema_revision,
                         compiled_set_digest, member_count, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        surface_set.installation_id().as_str(),
                        i64::try_from(surface_set.surface_revision().get()).unwrap(),
                        surface_set.package_revision_ref().as_str(),
                        i64::try_from(surface_set.schema_revision().get()).unwrap(),
                        surface_set.compiled_set_digest().as_str(),
                        i64::try_from(surface_set.surfaces().len()).unwrap(),
                        compiled.created_at.to_rfc3339(),
                    ],
                )?;
                for surface in surface_set.surfaces().values() {
                    let binding = surface.binding();
                    transaction.execute(
                        "INSERT INTO app_surface_generation_members (
                             installation_id, revision, view_id, app_local_route,
                             canonical_host_route, compiled_view_digest, binding_json,
                             envelope_json, created_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            binding.installation_id.as_str(),
                            i64::try_from(binding.surface_revision.get()).unwrap(),
                            binding.view_id.as_str(),
                            binding.app_local_route.as_str(),
                            binding.canonical_host_route.as_str(),
                            binding.compiled_view_digest.as_str(),
                            serde_json::to_vec(binding).unwrap(),
                            serde_json::to_vec(surface.envelope()).unwrap(),
                            compiled.created_at.to_rfc3339(),
                        ],
                    )?;
                }
                transaction.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        seeded_package_ref
    }

    pub fn compiled_schema() -> (AppDigest, AppSchemaRevision) {
        let manifest = manifest();
        let digest = canonical_entity_schema_digest(&manifest).unwrap();
        let compiled = compile_app_schema(
            &manifest,
            AppInstallationId::parse("install_1").unwrap(),
            reference("package-revision:reading-list"),
            AppRevision::new(1).unwrap(),
            policy(),
            super::super::records::AppSchemaCompatibility::Initial,
            None,
            &digest,
            Utc.with_ymd_and_hms(2026, 8, 16, 0, 0, 1).unwrap(),
        )
        .unwrap();
        (digest, compiled.into_revision())
    }

    fn mixed_field_policy_schema() -> AppSchemaRevision {
        let manifest = parse_app_manifest_yaml(
            br#"
name: mixed-policy-list
version: 0.1.0
description: Exercises query-influence policy joins.
metadata:
  magician:
    skill_type: app
    app_manifest_version: "1.0"
    app_sdk_version: "1"
app:
  compatibility: { magician_contract: "1" }
  data_policy:
    defaults:
      classification_floor: personal
      model_processing: local_only
      personal_agent_access: approved_projection
      memory_promotion: denied
      external_egress: denied
  entities:
    item:
      fields:
        title: { type: text, required: true }
        secret_rank:
          type: integer
          required: true
          data_policy: { classification_floor: sensitive }
  views:
    items:
      entity: item
      kind: table
      route: /
      columns: [title, secret_rank]
  workflows: {}
  actions: {}
  resources:
    per_run: { max_tokens: 1000, max_cost_usd: 0.25, max_active_seconds: 60 }
    monthly: { max_tokens: 10000, max_cost_usd: 5.00 }
    storage: { max_records: 10000, max_bytes: 10485760 }
  dependencies: { procedure_skills: [], capabilities: [] }
  assets: []
"#,
            &super::super::manifest::AppPackageLimits::default(),
        )
        .unwrap()
        .manifest()
        .clone();
        let digest = canonical_entity_schema_digest(&manifest).unwrap();
        compile_app_schema(
            &manifest,
            AppInstallationId::parse("install_1").unwrap(),
            reference("package-revision:reading-list"),
            AppRevision::new(1).unwrap(),
            policy(),
            super::super::records::AppSchemaCompatibility::Initial,
            None,
            &digest,
            Utc.with_ymd_and_hms(2026, 8, 16, 0, 0, 1).unwrap(),
        )
        .unwrap()
        .into_revision()
    }

    pub async fn seed_records(registry: &AppRegistryService) {
        let authenticated = authenticated_scope("anonymous", "default");
        registry
            .execute_scoped_write(&authenticated, &time(3), move |connection, _| {
                let transaction = connection.transaction()?;
                transaction.execute(
                    "INSERT INTO app_dataset_generations (
                         installation_id, current_generation, updated_at
                     ) VALUES (?1, 1, ?2)",
                    params!["install_1", time(3).to_rfc3339()],
                )?;
                for (sequence, record_id, title, status, parent) in [
                    (1_i64, "record_a", "Alpha", "open", None),
                    (2_i64, "record_b", "Beta", "done", Some("record_a")),
                ] {
                    let mut payload = serde_json::json!({"title": title, "status": status});
                    if let Some(parent) = parent {
                        payload["parent"] = serde_json::json!(parent);
                    }
                    let payload_json = serde_json::to_vec(&payload).unwrap();
                    let payload_digest = AppDigest::blake3_canonical_json(&payload).unwrap();
                    let handling = policy();
                    let handling_value = serde_json::to_value(&handling).unwrap();
                    let handling_digest =
                        AppDigest::blake3_canonical_json(&handling_value).unwrap();
                    let handling_json = serde_json::to_vec(&handling).unwrap();
                    let provenance_json =
                        serde_json::to_vec(&super::super::records::AppRecordProvenance {
                            actor_kind: super::super::records::AppRecordActorKind::User,
                            actor_id: reference("actor:owner"),
                            execution_id: None,
                            output_revision: None,
                            mutation_receipt_id: None,
                            source_artifact_refs: Vec::new(),
                            citation_refs: Vec::new(),
                        })
                        .unwrap();
                    transaction.execute(
                        "INSERT INTO app_record_revisions (
                             installation_id, entity_name, record_id, record_revision,
                             dataset_generation, schema_revision, payload_digest,
                             payload_json, handling_policy_digest, handling_policy_json,
                             provenance_json, created_at, updated_at, deleted_at
                         ) VALUES (?1, 'item', ?2, 1, 1, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?8, NULL)",
                        params![
                            "install_1",
                            record_id,
                            payload_digest.as_str(),
                            payload_json,
                            handling_digest.as_str(),
                            handling_json,
                            provenance_json,
                            time(3).to_rfc3339(),
                        ],
                    )?;
                    transaction.execute(
                        "INSERT INTO app_record_heads (
                             installation_id, entity_name, record_id, record_revision,
                             dataset_generation, schema_revision, change_seq, deleted_at
                         ) VALUES (?1, 'item', ?2, 1, 1, 1, ?3, NULL)",
                        params!["install_1", record_id, sequence],
                    )?;
                    for (field, kind, value) in
                        [("title", "text", title), ("status", "enum", status)]
                    {
                        transaction.execute(
                            "INSERT INTO app_scalar_indexes (
                                 installation_id, entity_name, field_path, record_id,
                                 record_revision, value_kind, text_value, order_key_asc, order_key_desc
                             ) VALUES (?1, 'item', ?2, ?3, 1, ?4, ?5, ?6, ?7)",
                            params!["install_1", field, record_id, kind, value,
                                super::super::indexed_snapshot::order_key(kind, Some(value), None, false)?,
                                super::super::indexed_snapshot::order_key(kind, Some(value), None, true)?],
                        )?;
                    }
                    transaction.execute(
                        "INSERT INTO app_text_search (
                             installation_id, entity_name, field_path, record_id,
                             record_revision, search_text
                         ) VALUES (?1, 'item', 'title', ?2, 1, ?3)",
                        params!["install_1", record_id, title],
                    )?;
                    if let Some(parent) = parent {
                        transaction.execute(
                            "INSERT INTO app_scalar_indexes (
                                 installation_id, entity_name, field_path, record_id,
                                 record_revision, value_kind, text_value, order_key_asc, order_key_desc
                             ) VALUES (?1, 'item', 'parent', ?2, 1, 'reference', ?3, ?4, ?5)",
                            params!["install_1", record_id, parent,
                                super::super::indexed_snapshot::order_key("reference", Some(parent), None, false)?,
                                super::super::indexed_snapshot::order_key("reference", Some(parent), None, true)?],
                        )?;
                    }
                }
                transaction.execute(
                    "INSERT INTO app_storage_usage (
                         installation_id, record_count, revision_count,
                         payload_bytes, attachment_bytes, updated_at
                     )
                     SELECT 'install_1',
                            (SELECT COUNT(*) FROM app_record_heads
                              WHERE installation_id = 'install_1' AND deleted_at IS NULL),
                            COUNT(*), COALESCE(SUM(length(payload_json)), 0), 0, ?1
                       FROM app_record_revisions
                      WHERE installation_id = 'install_1'",
                    params![time(3).to_rfc3339()],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn absent_scope_resolution_is_lazy() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let store = AppEntityStoreService::new(registry);
        let result = store
            .active_schema(
                &authenticated_scope("anonymous", "default"),
                &AppInstallationId::parse("install_missing").unwrap(),
                time(3),
            )
            .await
            .unwrap();
        assert!(result.is_none());
        assert!(!temporary.path().join("scopes").exists());
    }

    #[tokio::test]
    async fn exact_enabled_schema_resolves_and_disabled_installation_fails_closed() {
        for status in [
            AppInstallationStatus::Enabled,
            AppInstallationStatus::Disabled,
        ] {
            let temporary = canonical_tempdir();
            let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
            let (digest, schema) = compiled_schema();
            seed_enabled_installation(&registry, digest, schema, status).await;
            let result = AppEntityStoreService::new(registry)
                .active_schema(
                    &authenticated_scope("anonymous", "default"),
                    &AppInstallationId::parse("install_1").unwrap(),
                    time(3),
                )
                .await;
            if status == AppInstallationStatus::Enabled {
                let active = result.unwrap().unwrap();
                assert_eq!(active.schema_revision(), AppRevision::new(1).unwrap());
                assert!(active
                    .runtime_contract(&AppName::parse("item").unwrap())
                    .is_some());
            } else {
                assert!(matches!(
                    result,
                    Err(AppEntityStoreError::InstallationNotEnabled(
                        AppInstallationStatus::Disabled
                    ))
                ));
            }
        }
    }

    #[tokio::test]
    async fn runtime_authority_snapshot_binds_installation_and_active_schema_together() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled).await;
        let snapshot = AppEntityStoreService::new(registry)
            .runtime_authority_snapshot(
                &authenticated_scope("anonymous", "default"),
                &AppInstallationId::parse("install_1").unwrap(),
                None,
                time(3),
            )
            .await
            .unwrap();
        let (installation, active) = snapshot.into_parts();
        assert_eq!(installation.installation_id, *active.installation_id());
        assert_eq!(
            installation.lifecycle.generation,
            active.installation_generation()
        );
        assert_eq!(installation.grant_revision, Some(active.grant_revision()));
        assert_eq!(
            installation.active_schema_revision,
            Some(active.schema_revision())
        );
    }

    #[tokio::test]
    async fn package_schema_digest_mismatch_fails_closed() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (_, schema) = compiled_schema();
        seed_enabled_installation(
            &registry,
            AppDigest::blake3(b"wrong-schema"),
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;

        assert!(matches!(
            AppEntityStoreService::new(registry)
                .active_schema(
                    &authenticated_scope("anonymous", "default"),
                    &AppInstallationId::parse("install_1").unwrap(),
                    time(3),
                )
                .await,
            Err(AppEntityStoreError::PackageSchemaDigestMismatch)
        ));
    }

    #[tokio::test]
    async fn deterministic_query_pages_use_opaque_server_snapshot_cursors() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        let package_ref =
            seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled)
                .await;
        seed_records(&registry).await;
        let store = AppEntityStoreService::new(registry);
        let authenticated = authenticated_scope("anonymous", "default");
        let mut request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![
                AppFieldPath::parse("title").unwrap(),
                AppFieldPath::parse("status").unwrap(),
            ],
            predicate: None,
            order: vec![AppQueryOrder {
                field: AppFieldPath::parse("status").unwrap(),
                direction: AppOrderDirection::Ascending,
            }],
            cursor: None,
            limit: 1,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("surface").unwrap(),
        };
        let fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref.clone(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        let first = store
            .query(&authenticated, fence, request.clone(), time(3))
            .await
            .unwrap();
        assert_eq!(first.envelope.value[0].record_id.as_str(), "record_b");
        let first_retry_fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref.clone(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        let first_retry = store
            .query(&authenticated, first_retry_fence, request.clone(), time(3))
            .await
            .unwrap();
        assert_eq!(first_retry.envelope.value, first.envelope.value);
        assert_eq!(first_retry.next_cursor, first.next_cursor);
        request.cursor = first.next_cursor;
        let fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref.clone(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        let second = store
            .query(&authenticated, fence, request.clone(), time(4))
            .await
            .unwrap();
        assert_eq!(second.envelope.value[0].record_id.as_str(), "record_a");
        assert!(second.next_cursor.is_none());

        // A response can be lost after cursor persistence. Retrying the same
        // opaque cursor must still recover the same logical page rather than
        // strand the client behind a deleted cursor.
        let retry_fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        let retry = store
            .query(&authenticated, retry_fence, request, time(5))
            .await
            .unwrap();
        assert_eq!(retry.envelope.value, second.envelope.value);
        assert_eq!(retry.next_cursor, second.next_cursor);
    }

    #[tokio::test]
    async fn keyset_owner_pages_preserve_authority_and_survive_deletion_of_the_anchor() {
        use super::super::{
            entity_adapter::AppEntityAdapterService,
            models::{
                AppExpectedRecordRevision, AppMutationAtomicity, AppMutationCommand,
                AppMutationOperation, AppQueryPagination,
            },
        };
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled).await;
        let fixture_scope = authenticated_scope("anonymous", "default");
        // Keep test credentials valid while independently testing the cursor's
        // 24-hour lease; the shared fixture's login expires after 30 seconds.
        let authenticated = AuthenticatedAppScope::from_verified_session(
            fixture_scope.scope().clone(),
            fixture_scope.scope_binding_ref().clone(),
            reference("actor:owner"),
            reference("session:1"),
            fixture_scope.authentication_revision(),
            time(0),
            time(0) + chrono::Duration::days(2),
        )
        .unwrap();
        let adapter = AppEntityAdapterService::new(registry.clone());
        let mut request = AppQueryRequest {
            pagination: AppQueryPagination::Keyset,
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
            limit: 1,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("surface").unwrap(),
        };
        let empty = adapter
            .owner_query(&authenticated, request.clone(), time(2))
            .await
            .unwrap();
        assert!(empty.envelope.value.is_empty());
        assert!(empty.next_cursor.is_none());
        let store = AppEntityStoreService::new(registry.clone());
        let active = store
            .active_schema(&authenticated, &request.source_installation_id, time(2))
            .await
            .unwrap()
            .unwrap();
        assert!(active.supports_keyset_query(&request));
        let mut complex = request.clone();
        complex.order.push(complex.order[0].clone());
        assert!(!active.supports_keyset_query(&complex));
        seed_records(&registry).await;
        let context_fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            active.installation_generation(),
            active.package_revision_ref().clone(),
            active.schema_revision(),
            active.grant_revision(),
        );
        let context_page = store
            .query_page_without_continuation(
                &authenticated,
                context_fence,
                request.clone(),
                time(3),
            )
            .await
            .unwrap();
        assert_eq!(
            context_page.page().envelope.value[0].record_id.as_str(),
            "record_b"
        );
        assert!(context_page.page().next_cursor.is_none());
        let cursor_count: i64 = registry.execute_scoped_read(&authenticated, &time(3), |connection, _| {
            connection.query_row("SELECT (SELECT COUNT(*) FROM app_query_cursors) + (SELECT COUNT(*) FROM app_keyset_cursors)", [], |row| row.get(0)).map_err(Into::into)
        }).await.unwrap().unwrap();
        assert_eq!(
            cursor_count, 0,
            "bounded context reads must not consume cursor capacity"
        );
        // An installation full of abandoned first-page cursors must still
        // admit a new reader and let that reader continue at cache capacity.
        registry.execute_scoped_write(&authenticated, &time(3), |connection, _| {
            let transaction = connection.transaction()?;
            for ordinal in 0..MAX_QUERY_CURSORS_PER_INSTALLATION {
                transaction.execute("INSERT INTO app_keyset_cursors VALUES (?1,'install_1',2,?2,1,1,?3,?3,?1,?4)",
                    params![format!("cursor:abandoned:{ordinal:03}"), package_revision_ref().as_str(),
                        b"{}".as_slice(), (time(3) + chrono::Duration::hours(24)).to_rfc3339()])?;
            }
            transaction.commit()?;
            Ok(())
        }).await.unwrap();
        let first = adapter
            .owner_query(&authenticated, request.clone(), time(3))
            .await
            .unwrap();
        assert_eq!(first.envelope.value[0].record_id.as_str(), "record_b");
        assert!(first.next_cursor.is_some());
        request.cursor = first.next_cursor;
        let mut changed_query = request.clone();
        changed_query.order[0].direction = AppOrderDirection::Descending;
        assert!(adapter
            .owner_query(&authenticated, changed_query, time(4))
            .await
            .is_err());
        let mut changed_mode = request.clone();
        changed_mode.pagination = AppQueryPagination::Snapshot;
        assert!(adapter
            .owner_query(&authenticated, changed_mode, time(4))
            .await
            .is_err());
        assert!(adapter
            .owner_query(
                &authenticated_scope("someone_else", "default"),
                request.clone(),
                time(4)
            )
            .await
            .is_err());
        adapter
            .owner_mutate(
                &authenticated,
                &request.source_installation_id,
                AppMutationCommand {
                    protocol_version: AppProtocolVersion::V1,
                    idempotency_key: reference("mutation:keyset-delete-anchor"),
                    atomicity: AppMutationAtomicity::AllOrNothing,
                    expected_schema_revision: AppRevision::new(1).unwrap(),
                    operations: vec![
                        AppMutationOperation::Delete {
                            entity: request.entity.clone(),
                            record_id: AppRecordId::parse("record_b").unwrap(),
                        },
                        AppMutationOperation::Create {
                            entity: request.entity.clone(),
                            record_id: Some(AppRecordId::parse("record_c").unwrap()),
                            temporary_id: AppName::parse("created_c").unwrap(),
                            payload: serde_json::json!({"title":"Charlie", "status":"open"}),
                        },
                    ],
                    expected_record_revisions: vec![AppExpectedRecordRevision {
                        entity: request.entity.clone(),
                        record_id: AppRecordId::parse("record_b").unwrap(),
                        revision: AppRevision::new(1).unwrap(),
                    }],
                },
                time(4),
            )
            .await
            .unwrap();
        let continued_at = time(5) + chrono::Duration::minutes(11);
        let second = adapter
            .owner_query(&authenticated, request.clone(), continued_at)
            .await
            .unwrap();
        assert_eq!(second.envelope.value[0].record_id.as_str(), "record_a");
        let replay = adapter
            .owner_query(&authenticated, request.clone(), continued_at)
            .await
            .unwrap();
        assert_eq!(replay, second);
        request.cursor = second.next_cursor;
        let third = adapter
            .owner_query(
                &authenticated,
                request.clone(),
                continued_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap();
        assert_eq!(third.envelope.value[0].record_id.as_str(), "record_c");
        assert!(third.next_cursor.is_none());
        assert!(matches!(
            adapter
                .owner_query(
                    &authenticated,
                    request,
                    continued_at + chrono::Duration::hours(25)
                )
                .await,
            Err(super::super::entity_adapter::AppEntityAdapterError::Store(
                AppEntityStoreError::Query(AppQuerySemanticsError::CursorExpired)
            ))
        ));
        let (snapshots, cursors, largest_boundary): (i64,i64,i64) = registry.execute_scoped_read(&authenticated, &time(7), |connection, _| {
            connection.query_row("SELECT (SELECT COUNT(*) FROM app_query_cursors), COUNT(*), MAX(length(boundary_json)) FROM app_keyset_cursors", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).map_err(Into::into)
        }).await.unwrap().unwrap();
        assert_eq!(snapshots, 0);
        assert_eq!(cursors, MAX_QUERY_CURSORS_PER_INSTALLATION);
        let live_chain_cursors: i64 = registry.execute_scoped_read(&authenticated, &time(7), |connection, _| {
            connection.query_row("SELECT COUNT(*) FROM app_keyset_cursors WHERE cursor_ref NOT LIKE 'cursor:abandoned:%'", [], |row| row.get(0)).map_err(Into::into)
        }).await.unwrap().unwrap();
        assert_eq!(live_chain_cursors, 2);
        assert!(largest_boundary < 256);
    }

    #[tokio::test]
    async fn query_snapshot_survives_append_between_read_and_cursor_publication() {
        use super::super::{
            entity_adapter::AppEntityAdapterService,
            models::{
                AppExpectedRecordRevision, AppMutationAtomicity, AppMutationCommand,
                AppMutationOperation,
            },
        };
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        let package_ref =
            seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled)
                .await;
        seed_records(&registry).await;
        let authenticated = authenticated_scope("anonymous", "default");
        let store = AppEntityStoreService::new(registry.clone());
        let adapter = AppEntityAdapterService::new(registry.clone());
        let mut request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit: 1,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("surface").unwrap(),
        };
        let fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        let read_request = request.clone();
        let scope_binding = authenticated.scope_binding_ref().clone();
        let auth_revision = authenticated.authentication_revision();
        let prepared = registry
            .execute_scoped_read(&authenticated, &time(3), move |connection, scope| {
                Ok(prepare_query_page(
                    connection,
                    scope,
                    &scope_binding,
                    auth_revision,
                    fence,
                    &read_request,
                    time(3),
                ))
            })
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            prepared.page.envelope.value[0].record_id.as_str(),
            "record_a"
        );

        // Exact production race: the read has finished, but its opaque cursor
        // has not been published. A real owner mutation appends another row.
        adapter
            .owner_mutate(
                &authenticated,
                &request.source_installation_id,
                AppMutationCommand {
                    protocol_version: AppProtocolVersion::V1,
                    idempotency_key: reference("mutation:append-during-pagination"),
                    atomicity: AppMutationAtomicity::AllOrNothing,
                    expected_schema_revision: AppRevision::new(1).unwrap(),
                    operations: vec![AppMutationOperation::Create {
                        entity: request.entity.clone(),
                        temporary_id: AppName::parse("new_item").unwrap(),
                        record_id: Some(AppRecordId::parse("record_c").unwrap()),
                        payload: serde_json::json!({"title":"Charlie", "status":"open"}),
                    }],
                    expected_record_revisions: Vec::new(),
                },
                time(4),
            )
            .await
            .unwrap();
        store
            .persist_cursor(&authenticated, prepared.cursor_write.unwrap(), time(4))
            .await
            .unwrap();
        request.cursor = prepared.page.next_cursor;
        let second = adapter
            .owner_query(&authenticated, request.clone(), time(5))
            .await
            .unwrap();
        assert_eq!(second.envelope.value[0].record_id.as_str(), "record_b");
        assert!(
            second.next_cursor.is_none(),
            "new rows do not enter the retained membership"
        );
        let mut fresh = request.clone();
        fresh.cursor = None;
        fresh.limit = 10;
        assert_eq!(
            adapter
                .owner_query(&authenticated, fresh, time(5))
                .await
                .unwrap()
                .envelope
                .value
                .len(),
            3
        );

        // A changed row cannot be disclosed through old snapshot evidence.
        adapter
            .owner_mutate(
                &authenticated,
                &request.source_installation_id,
                AppMutationCommand {
                    protocol_version: AppProtocolVersion::V1,
                    idempotency_key: reference("mutation:edit-snapshot-row"),
                    atomicity: AppMutationAtomicity::AllOrNothing,
                    expected_schema_revision: AppRevision::new(1).unwrap(),
                    operations: vec![AppMutationOperation::Update {
                        entity: request.entity.clone(),
                        record_id: AppRecordId::parse("record_b").unwrap(),
                        patch: serde_json::json!({"title":"Changed"}),
                    }],
                    expected_record_revisions: vec![AppExpectedRecordRevision {
                        entity: request.entity.clone(),
                        record_id: AppRecordId::parse("record_b").unwrap(),
                        revision: AppRevision::new(1).unwrap(),
                    }],
                },
                time(6),
            )
            .await
            .unwrap();
        assert!(matches!(
            adapter
                .owner_query(&authenticated, request.clone(), time(7))
                .await,
            Err(super::super::entity_adapter::AppEntityAdapterError::Store(
                AppEntityStoreError::CursorSnapshotUnavailable
            ))
        ));

        request.cursor = None;
        let fresh = adapter
            .owner_query(&authenticated, request.clone(), time(7))
            .await
            .unwrap();
        request.cursor = fresh.next_cursor;
        assert!(request.cursor.is_some());
        registry.execute_scoped_write(&authenticated, &time(7), |connection, _| {
            connection.execute("UPDATE app_dataset_generations SET current_generation = 1 WHERE installation_id = 'install_1'", [])?;
            Ok(())
        }).await.unwrap();
        assert!(matches!(
            adapter.owner_query(&authenticated, request, time(8)).await,
            Err(super::super::entity_adapter::AppEntityAdapterError::Store(
                AppEntityStoreError::StaleDatasetGeneration
            ))
        ));
    }

    #[tokio::test]
    async fn record_projection_revalidation_reopens_exact_current_row() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        let package_ref =
            seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled)
                .await;
        seed_records(&registry).await;
        let store = AppEntityStoreService::new(registry);
        let authenticated = authenticated_scope("anonymous", "default");
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![
                AppFieldPath::parse("title").unwrap(),
                AppFieldPath::parse("status").unwrap(),
            ],
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit: 1,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("app_composition").unwrap(),
        };
        let fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref.clone(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        let page = store
            .query(&authenticated, fence, request.clone(), time(3))
            .await
            .unwrap();
        let projection = page.envelope.value[0].clone();
        let revalidation_fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref.clone(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        let proof = store
            .revalidate_record_projection(
                &authenticated,
                revalidation_fence,
                request.clone(),
                &page.envelope,
                &projection,
                time(4),
            )
            .await
            .unwrap();
        assert_eq!(proof.projection(), &projection);
        assert_eq!(&proof.source_ref().fields, &request.select);

        let page_fence = || {
            AppStoreAuthorityFence::for_store_test(
                &request,
                authenticated.scope_binding_ref().clone(),
                authenticated.authentication_revision(),
                2,
                package_ref.clone(),
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
            )
        };
        store
            .revalidate_record_projections(
                &authenticated,
                vec![page_fence()],
                request.clone(),
                &page.envelope,
                time(4),
            )
            .await
            .unwrap();
        assert!(store
            .revalidate_record_projections(
                &authenticated,
                Vec::new(),
                request.clone(),
                &page.envelope,
                time(4),
            )
            .await
            .is_err());
        let mut changed_page = page.envelope.clone();
        changed_page.value[0].fields.insert(
            AppFieldPath::parse("title").unwrap(),
            Value::String("forged".to_owned()),
        );
        changed_page.content_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&changed_page.value).unwrap())
                .unwrap();
        assert!(matches!(
            store
                .revalidate_record_projections(
                    &authenticated,
                    vec![page_fence()],
                    request.clone(),
                    &changed_page,
                    time(4),
                )
                .await,
            Err(AppEntityStoreError::StaleRecordProjection)
        ));

        let mut tampered = projection;
        tampered.fields.insert(
            AppFieldPath::parse("title").unwrap(),
            Value::String("forged".to_string()),
        );
        let tampered_fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        assert!(matches!(
            store
                .revalidate_record_projection(
                    &authenticated,
                    tampered_fence,
                    request,
                    &page.envelope,
                    &tampered,
                    time(4),
                )
                .await,
            Err(AppEntityStoreError::InvalidRecordProjection)
        ));
    }

    #[tokio::test]
    async fn cursor_chains_retain_one_snapshot_and_only_the_active_replay_window() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        let package_ref =
            seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled)
                .await;
        seed_records(&registry).await;
        let authenticated = authenticated_scope("anonymous", "default");
        registry
            .execute_scoped_write(&authenticated, &time(3), |connection, _| {
                let transaction = connection.transaction()?;
                for (sequence, record_id) in [(3_i64, "record_c"), (4, "record_d"), (5, "record_e")]
                {
                    transaction.execute(
                        "INSERT INTO app_record_revisions (
                             installation_id, entity_name, record_id, record_revision,
                             dataset_generation, schema_revision, payload_digest,
                             payload_json, handling_policy_digest, handling_policy_json,
                             provenance_json, created_at, updated_at, deleted_at
                         ) SELECT installation_id, entity_name, ?1, record_revision,
                                  dataset_generation, schema_revision, payload_digest,
                                  payload_json, handling_policy_digest, handling_policy_json,
                                  provenance_json, created_at, updated_at, deleted_at
                             FROM app_record_revisions
                            WHERE installation_id = 'install_1'
                              AND entity_name = 'item'
                              AND record_id = 'record_a'",
                        params![record_id],
                    )?;
                    transaction.execute(
                        "INSERT INTO app_record_heads (
                             installation_id, entity_name, record_id, record_revision,
                             dataset_generation, schema_revision, change_seq, deleted_at
                         ) SELECT installation_id, entity_name, ?1, record_revision,
                                  dataset_generation, schema_revision, ?2, deleted_at
                             FROM app_record_heads
                            WHERE installation_id = 'install_1'
                              AND entity_name = 'item'
                              AND record_id = 'record_a'",
                        params![record_id, sequence],
                    )?;
                }
                transaction.commit()?;
                Ok(())
            })
            .await
            .unwrap();

        let store = AppEntityStoreService::new(registry.clone());
        let mut request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit: 1,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("surface").unwrap(),
        };
        let mut replay_request = None;
        let mut replay_page = None;
        for page_index in 0..4 {
            let fence = AppStoreAuthorityFence::for_store_test(
                &request,
                authenticated.scope_binding_ref().clone(),
                authenticated.authentication_revision(),
                2,
                package_ref.clone(),
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
            );
            let page = store
                .query(&authenticated, fence, request.clone(), time(4))
                .await
                .unwrap();
            assert!(page.next_cursor.is_some());
            if page_index == 3 {
                replay_request = Some(request.clone());
                replay_page = Some(page.clone());
            }
            request.cursor = page.next_cursor;
        }

        let (cursor_count, retained_snapshots, snapshot_chains): (i64, i64, i64) = registry
            .execute_scoped_read(&authenticated, &time(4), |connection, _| {
                connection
                    .query_row(
                        "SELECT COUNT(*),
                                SUM(CASE WHEN length(snapshot_json) > 0 THEN 1 ELSE 0 END),
                                COUNT(DISTINCT snapshot_ref)
                           FROM app_query_cursors
                          WHERE installation_id = 'install_1'",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .map_err(super::super::registry::AppRegistryError::from)
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cursor_count, 3);
        assert_eq!(retained_snapshots, 1);
        assert_eq!(snapshot_chains, 1);

        let replay_request = replay_request.unwrap();
        let fence = AppStoreAuthorityFence::for_store_test(
            &replay_request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        let replay = store
            .query(&authenticated, fence, replay_request, time(5))
            .await
            .unwrap();
        let original = replay_page.unwrap();
        assert_eq!(replay.envelope.value, original.envelope.value);
        assert_eq!(replay.next_cursor, original.next_cursor);
    }

    #[tokio::test]
    async fn managed_dataset_uses_deterministic_text_index_and_rechecks_the_predicate() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        let package_ref =
            seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled)
                .await;
        seed_records(&registry).await;
        let store = AppEntityStoreService::new(registry);
        let authenticated = authenticated_scope("anonymous", "default");
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: Some(AppPredicate {
                root: 0,
                nodes: vec![AppPredicateNode::Compare {
                    field: AppFieldPath::parse("title").unwrap(),
                    operator: AppComparisonOperator::Contains,
                    value: serde_json::json!("ph"),
                }],
            }),
            order: vec![AppQueryOrder {
                field: AppFieldPath::parse("status").unwrap(),
                direction: AppOrderDirection::Ascending,
            }],
            cursor: None,
            limit: 25,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("surface").unwrap(),
        };
        let fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );

        let page = store
            .query(&authenticated, fence, request, time(3))
            .await
            .unwrap();
        assert_eq!(page.envelope.value.len(), 1);
        assert_eq!(page.envelope.value[0].record_id.as_str(), "record_a");
    }

    #[test]
    fn conjunction_uses_a_direct_indexed_child_without_weakening_full_predicate_recheck() {
        let (_, schema) = compiled_schema();
        let contracts =
            super::super::schema_compiler::runtime_contracts_from_revision(&schema).unwrap();
        let runtime = contracts.get(&AppName::parse("item").unwrap()).unwrap();
        let predicate = AppPredicate {
            root: 0,
            nodes: vec![
                AppPredicateNode::All {
                    children: vec![1, 2],
                },
                AppPredicateNode::Compare {
                    field: AppFieldPath::parse("status").unwrap(),
                    operator: AppComparisonOperator::Equal,
                    value: serde_json::json!("done"),
                },
                AppPredicateNode::Compare {
                    field: AppFieldPath::parse("title").unwrap(),
                    operator: AppComparisonOperator::StartsWith,
                    value: serde_json::json!("B"),
                },
            ],
        };
        let lookup = indexed_predicate_lookup(&predicate, runtime)
            .unwrap()
            .expect("the direct equality child is indexed");
        assert_eq!(lookup.2.as_str(), "status");

        let malformed = AppPredicate {
            root: 0,
            nodes: vec![AppPredicateNode::All { children: vec![1] }],
        };
        assert!(matches!(
            indexed_predicate_lookup(&malformed, runtime),
            Err(AppEntityStoreError::InvalidPredicate)
        ));
    }

    #[tokio::test]
    async fn declared_relation_expansion_is_bounded_and_source_linked() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        let package_ref =
            seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled)
                .await;
        seed_records(&registry).await;
        let store = AppEntityStoreService::new(registry);
        let authenticated = authenticated_scope("anonymous", "default");
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: Some(AppPredicate {
                root: 0,
                nodes: vec![AppPredicateNode::Compare {
                    field: AppFieldPath::parse("status").unwrap(),
                    operator: AppComparisonOperator::Equal,
                    value: serde_json::json!("done"),
                }],
            }),
            order: Vec::new(),
            cursor: None,
            limit: 25,
            relation_expansions: vec![AppRelationExpansion {
                relation: AppName::parse("parent").unwrap(),
                select: vec![AppFieldPath::parse("title").unwrap()],
                max_depth: 2,
                max_rows: 2,
            }],
            purpose: AppName::parse("surface").unwrap(),
        };
        let fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );

        let page = store
            .query(&authenticated, fence, request, time(3))
            .await
            .unwrap();
        assert_eq!(page.envelope.value.len(), 1);
        assert_eq!(page.envelope.source_refs.len(), 2);
        let influence_fields = page
            .envelope
            .source_refs
            .iter()
            .flat_map(|source| source.fields.iter().map(ToString::to_string))
            .collect::<BTreeSet<_>>();
        assert!(influence_fields.contains("title"));
        assert!(influence_fields.contains("status"));
        assert!(influence_fields.contains("parent"));
        assert_eq!(
            page.envelope.value[0].fields[&AppFieldPath::parse("parent").unwrap()][0]["fields"]
                ["title"],
            serde_json::json!("Alpha")
        );
    }

    #[tokio::test]
    async fn read_fails_closed_when_record_policy_digest_is_tampered() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        let package_ref =
            seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled)
                .await;
        seed_records(&registry).await;
        let authenticated = authenticated_scope("anonymous", "default");
        registry
            .execute_scoped_write(&authenticated, &time(3), |connection, _| {
                connection.execute(
                    "UPDATE app_record_revisions SET handling_policy_digest = ?1
                     WHERE installation_id = 'install_1' AND record_id = 'record_a'",
                    params![AppDigest::blake3(b"tampered").as_str()],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit: 25,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("surface").unwrap(),
        };
        let fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );

        assert!(matches!(
            AppEntityStoreService::new(registry)
                .query(&authenticated, fence, request, time(3))
                .await,
            Err(AppEntityStoreError::CorruptRecord)
        ));
    }

    #[tokio::test]
    async fn predicate_and_order_fields_join_their_policy_even_when_not_projected() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled).await;
        let authenticated = authenticated_scope("anonymous", "default");
        let store = AppEntityStoreService::new(registry);
        let mut active = store
            .active_schema(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                time(3),
            )
            .await
            .unwrap()
            .unwrap();
        active.schema = mixed_field_policy_schema();
        active.runtime_contracts = runtime_contracts_from_revision(&active.schema).unwrap();
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: Some(AppPredicate {
                root: 0,
                nodes: vec![AppPredicateNode::Compare {
                    field: AppFieldPath::parse("secret_rank").unwrap(),
                    operator: AppComparisonOperator::GreaterThan,
                    value: serde_json::json!(10),
                }],
            }),
            order: vec![AppQueryOrder {
                field: AppFieldPath::parse("secret_rank").unwrap(),
                direction: AppOrderDirection::Ascending,
            }],
            cursor: None,
            limit: 100,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("personal_agent_query").unwrap(),
        };
        let runtime = active.runtime_contract(&request.entity).unwrap();
        assert_eq!(
            query_policy_influence_fields(&request),
            vec![
                AppFieldPath::parse("secret_rank").unwrap(),
                AppFieldPath::parse("title").unwrap(),
            ]
        );
        let joined = selected_policy(&active, &request, runtime).unwrap();
        assert_eq!(
            joined.classification_floor,
            AppDataClassification::Sensitive
        );
    }

    #[test]
    fn reference_qualification_snapshot_scale_round_trips_through_cursor_storage() {
        let reference_rows = usize::try_from(
            magician_apps::apps::benchmark_fixtures::APP_BENCHMARK_ACTIVE_INSTALLATION_RECORDS,
        )
        .unwrap();
        assert!(MAX_QUERY_SNAPSHOT_ROWS >= reference_rows);
        let snapshot = (0..reference_rows)
            .map(|index| SnapshotEntry {
                record_id: AppRecordId::parse(format!("record_{index}")).unwrap(),
                record_revision: AppRevision::new(1).unwrap(),
            })
            .collect::<Vec<_>>();
        let encoded = encode_snapshot(&snapshot).unwrap();
        let decoded = decode_snapshot(&encoded).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[tokio::test]
    async fn installation_cursor_quota_fails_typed_before_unbounded_snapshot_retention() {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (digest, schema) = compiled_schema();
        let package_ref =
            seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled)
                .await;
        seed_records(&registry).await;
        let authenticated = authenticated_scope("anonymous", "default");
        registry
            .execute_scoped_write(&authenticated, &time(3), |connection, _| {
                let transaction = connection.transaction()?;
                for ordinal in 0..MAX_QUERY_CURSORS_PER_INSTALLATION {
                    let cursor_ref = format!("cursor:quota-{ordinal}");
                    transaction.execute(
                        "INSERT INTO app_query_cursors (
                             cursor_ref, installation_id, schema_revision, dataset_generation,
                             evidence_json, snapshot_json, snapshot_ref, next_offset,
                             created_at, expires_at
                         ) VALUES (?1, 'install_1', 1, 1, ?2, ?3, ?1, 1, ?4, ?5)",
                        params![
                            cursor_ref,
                            b"{}".as_slice(),
                            b"ASNP\x01".as_slice(),
                            time(3).to_rfc3339(),
                            time(30).to_rfc3339(),
                        ],
                    )?;
                }
                transaction.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        let request = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit: 1,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("surface").unwrap(),
        };
        let fence = AppStoreAuthorityFence::for_store_test(
            &request,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        assert!(matches!(
            AppEntityStoreService::new(registry)
                .query(&authenticated, fence, request, time(4))
                .await,
            Err(AppEntityStoreError::CursorCapacityExceeded)
        ));
    }
}
