//! Bounded, owner-facing discovery for enabled app installations.
//!
//! The shallow path reads only registry-owned directory metadata. Complete
//! schemas are loaded for exactly one selected installation and are filtered
//! through the same current grant, compiled schema and physical provider
//! policy used by personal-agent app-store reads.

use std::collections::{BTreeMap, BTreeSet};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, ErrorCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    boundary::{
        AppAgentProcessingClass, AppBoundaryError, AppPersonalAgentReadAuthority,
        AppStoreReadAudience,
    },
    entity_store::{ActiveAppEntitySchema, AppEntityStoreError, AppEntityStoreService},
    lifecycle::AppInstallationStatus,
    manifest::{
        AppManifestField, AppManifestInputSchema, AppManifestTrigger, AppManifestView,
        AppManifestViewKind, AppPackageManifest,
    },
    models::{
        decode_app_contract, AppContractError, AppContractLimits, AppDataClassification, AppDigest,
        AppFieldPath, AppInstallationId, AppModelProcessing, AppName, AppProtocolVersion,
    },
    package_staging::{AppPackageStager, AppPackageStagingError},
    policy::intersect_app_data_handling_policies,
    query_semantics::AppQueryScalarKind,
    records::{
        AppDataHandlingPolicy, AppGrantRevision, AppInstallation, AppPackageDirectoryMetadata,
        AppPackageRevision, AppPersonalAgentAccess, AppSchemaRevision, AppScope,
    },
    registry::{AppRegistryError, AppRegistryService},
    schema_compiler::restrict_policy,
    workflows::{declared_workflow_is_grant_eligible, AppWorkflowService},
};
use crate::magician_v2::json_traversal::exact_json_encoded_len;

pub const DEFAULT_APP_DISCOVERY_LIMIT: usize = 12;
pub const MAX_APP_DISCOVERY_LIMIT: usize = 24;
pub const MAX_APP_DISCOVERY_SEARCH_BYTES: usize = 128;
pub const MAX_APP_DISCOVERY_CURSOR_BYTES: usize = 512;
pub const MAX_APP_DISCOVERY_RESPONSE_BYTES: usize = 256 * 1_024;
pub const MAX_APP_DISCOVERY_CANDIDATE_SCAN: usize = 512;
pub const MAX_APP_DISCOVERY_RAW_SCAN_BYTES: usize = 8 * 1_024 * 1_024;
const APP_DISCOVERY_PROGRESS_INTERVAL_OPS: i32 = 1_000;
const MAX_APP_DISCOVERY_VM_STEPS: usize = 250_000;
const MAX_APP_DISCOVERY_SQL_MILLIS: u64 = 100;
// AppName is ASCII alphanumeric plus `_`, `-` and `.`. Under SQLite's binary
// collation `{` is therefore strictly above every normalized valid name.
const APP_DISCOVERY_ALL_NAMES_UPPER_BOUND: &str = "{";
pub const APP_DISCOVERY_UNAVAILABLE_CODE: &str = "app_discovery_unavailable";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDiscoveryListQuery {
    pub search: Option<String>,
    pub cursor: Option<String>,
    pub limit: usize,
}

impl AppDiscoveryListQuery {
    pub fn validate(&self) -> Result<(), AppDiscoveryError> {
        if self.limit == 0 || self.limit > MAX_APP_DISCOVERY_LIMIT {
            return Err(AppDiscoveryError::InvalidRequest(
                "discovery limit is outside the supported range",
            ));
        }
        if let Some(search) = self.search.as_deref() {
            let normalized = search.trim();
            if normalized.is_empty()
                || search.len() > MAX_APP_DISCOVERY_SEARCH_BYTES
                || search.chars().any(char::is_control)
                || !normalized
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(AppDiscoveryError::InvalidRequest(
                    "discovery prefix is too large or is not an app-name prefix",
                ));
            }
        }
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.len() > MAX_APP_DISCOVERY_CURSOR_BYTES)
        {
            return Err(AppDiscoveryError::InvalidCursor);
        }
        Ok(())
    }

    fn normalized_search(&self) -> String {
        self.search
            .as_deref()
            .map(str::trim)
            .filter(|search| !search.is_empty())
            .unwrap_or_default()
            .to_ascii_lowercase()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppPersonalAgentDiscoveryRequest {
    List(AppDiscoveryListQuery),
    Describe(AppInstallationId),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppPersonalAgentDiscoveryResult {
    List { page: AppDiscoveryPage },
    App { app: AppDiscoveredApp },
    Unavailable { error_code: &'static str },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDiscoverySummary {
    pub installation_id: AppInstallationId,
    pub name: AppName,
    pub package_version: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDiscoveryPage {
    pub protocol_version: AppProtocolVersion,
    pub apps: Vec<AppDiscoverySummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDiscoveredApp {
    pub protocol_version: AppProtocolVersion,
    pub installation_id: AppInstallationId,
    pub name: AppName,
    pub package_version: String,
    pub entities: Vec<AppDiscoveredEntity>,
    pub views: Vec<AppDiscoveredView>,
    pub actions: Vec<AppDiscoveredAction>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDiscoveredEntity {
    pub entity: AppName,
    pub search: bool,
    pub fields: Vec<AppDiscoveredField>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDiscoveredField {
    pub field: AppFieldPath,
    pub field_type: AppQueryScalarKind,
    pub required: bool,
    pub nullable: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<AppName>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDiscoveredView {
    pub view_id: AppName,
    pub entity: AppName,
    pub view_type: AppManifestViewKind,
    pub projected_fields: Vec<AppFieldPath>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppDiscoveredResultDelivery {
    TypedWhenEligible,
    RunHandleOnly,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDiscoveredAction {
    pub action_id: AppName,
    pub input_schema: AppDiscoveredObjectSchema,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<AppDiscoveredObjectSchema>,
    pub result_delivery: AppDiscoveredResultDelivery,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDiscoveredObjectSchema {
    pub fields: Vec<AppDiscoveredSchemaField>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDiscoveredSchemaField {
    pub field: AppName,
    pub field_type: AppQueryScalarKind,
    pub required: bool,
    pub nullable: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<AppName>,
}

#[derive(Debug, Error)]
pub enum AppDiscoveryError {
    #[error(transparent)]
    Boundary(#[from] AppBoundaryError),
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Staging(#[from] AppPackageStagingError),
    #[error(transparent)]
    EntityStore(#[from] AppEntityStoreError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid app discovery cursor")]
    InvalidCursor,
    #[error("invalid app discovery request: {0}")]
    InvalidRequest(&'static str),
    #[error("app installation is unavailable")]
    InstallationUnavailable,
    #[error("app discovery response exceeds its byte ceiling")]
    ResponseTooLarge,
    #[error("app discovery candidate scan reached its fixed ceiling")]
    CandidateScanLimitExceeded,
    #[error("app discovery raw scan reached its fixed byte ceiling")]
    RawScanByteLimitExceeded,
    #[error("corrupt app discovery state: {0}")]
    Corrupt(&'static str),
    #[error("failed to encode app discovery state: {0}")]
    Encoding(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct AppPersonalAgentDiscoveryService {
    registry: AppRegistryService,
    stager: AppPackageStager,
    entity_store: AppEntityStoreService,
}

impl AppPersonalAgentDiscoveryService {
    pub(crate) fn from_workflow(workflow: &AppWorkflowService) -> Self {
        Self {
            registry: workflow.registry_service(),
            stager: workflow.package_stager(),
            entity_store: workflow.entity_store_service(),
        }
    }

    pub async fn discover(
        &self,
        authenticated: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppPersonalAgentDiscoveryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppPersonalAgentDiscoveryResult, AppDiscoveryError> {
        authority.ensure_current(authenticated, &now)?;
        let metadata_processing = discovery_metadata_processing(authority.processing_class());
        let audience = AppStoreReadAudience::PersonalAgent {
            execution_ref: authority.execution_ref().clone(),
            processing_class: authority.processing_class(),
            maximum_classification: authority.maximum_classification(),
        };
        if !audience.permits_policy(AppDataClassification::Sensitive, metadata_processing) {
            return Err(AppDiscoveryError::InstallationUnavailable);
        }
        let result = match request {
            AppPersonalAgentDiscoveryRequest::List(query) => {
                AppPersonalAgentDiscoveryResult::List {
                    page: self.list(authenticated, &audience, query, now).await?,
                }
            },
            AppPersonalAgentDiscoveryRequest::Describe(installation_id) => {
                match self
                    .describe(authenticated, &audience, &installation_id, now)
                    .await
                {
                    Ok(app) => AppPersonalAgentDiscoveryResult::App { app },
                    Err(AppDiscoveryError::InstallationUnavailable) => {
                        AppPersonalAgentDiscoveryResult::Unavailable {
                            error_code: APP_DISCOVERY_UNAVAILABLE_CODE,
                        }
                    },
                    Err(error) => return Err(error),
                }
            },
        };
        enforce_response_bound(&result)?;
        Ok(result)
    }

    async fn list(
        &self,
        authenticated: &AuthenticatedAppScope,
        audience: &AppStoreReadAudience,
        query: AppDiscoveryListQuery,
        now: DateTime<Utc>,
    ) -> Result<AppDiscoveryPage, AppDiscoveryError> {
        query.validate()?;
        let audience = audience.clone();
        let page = self
            .registry
            .execute_scoped_typed_read(authenticated, &now, move |connection, scope| {
                list_discovery_snapshot(connection, scope, &audience, &query)
            })
            .await?
            .unwrap_or_else(empty_page);
        Ok(page)
    }

    async fn describe(
        &self,
        authenticated: &AuthenticatedAppScope,
        audience: &AppStoreReadAudience,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppDiscoveredApp, AppDiscoveryError> {
        let before = self
            .entity_store
            .active_schema(authenticated, installation_id, now)
            .await
            .map_err(|_| AppDiscoveryError::InstallationUnavailable)?
            .ok_or(AppDiscoveryError::InstallationUnavailable)?;
        if !grant_policy_permits_audience(&before.grant().granted_data_handling_policy, audience) {
            return Err(AppDiscoveryError::InstallationUnavailable);
        }
        let package = self
            .registry
            .package_revision(authenticated, before.package_revision_ref(), now)
            .await?
            .ok_or(AppDiscoveryError::InstallationUnavailable)?;
        let staged = self
            .stager
            .load_staged_package(authenticated, package.content_digest.clone(), now)
            .await?;

        // Package admission performs blocking I/O. Re-resolve the complete
        // active tuple afterwards so a concurrent disable/update cannot leave
        // stale schemas visible to the model.
        let active = self
            .entity_store
            .active_schema(authenticated, installation_id, now)
            .await
            .map_err(|_| AppDiscoveryError::InstallationUnavailable)?
            .ok_or(AppDiscoveryError::InstallationUnavailable)?;
        if active.installation_generation() != before.installation_generation()
            || active.package_revision_ref() != before.package_revision_ref()
            || active.schema_revision() != before.schema_revision()
            || active.grant_revision() != before.grant_revision()
        {
            return Err(AppDiscoveryError::InstallationUnavailable);
        }
        if !grant_policy_permits_audience(&active.grant().granted_data_handling_policy, audience) {
            return Err(AppDiscoveryError::InstallationUnavailable);
        }

        let manifest = staged.candidate().manifest().manifest();
        if manifest.version != package.semantic_version {
            return Err(AppDiscoveryError::Corrupt(
                "staged manifest version differs from the current package revision",
            ));
        }
        build_discovered_app(installation_id, &package, manifest, &active, audience)
    }
}

#[derive(Debug)]
struct AppDiscoveryCandidate {
    installation: AppInstallation,
    package: AppPackageRevision,
    metadata: AppPackageDirectoryMetadata,
    sort_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppDiscoveryCursor {
    version: u8,
    search_digest: AppDigest,
    sort_name: String,
    package_revision_ref: super::models::AppReference,
    installation_id: AppInstallationId,
}

fn empty_page() -> AppDiscoveryPage {
    AppDiscoveryPage {
        protocol_version: AppProtocolVersion::V1,
        apps: Vec::new(),
        next_cursor: None,
        has_more: false,
    }
}

fn grant_policy_permits_audience(
    policy: &AppDataHandlingPolicy,
    audience: &AppStoreReadAudience,
) -> bool {
    policy.personal_agent_access == AppPersonalAgentAccess::ApprovedProjection
        && audience.permits_policy(policy.classification_floor, policy.model_processing)
}

fn discovery_metadata_processing(processing_class: AppAgentProcessingClass) -> AppModelProcessing {
    match processing_class {
        AppAgentProcessingClass::Deterministic => AppModelProcessing::None,
        AppAgentProcessingClass::LocalModel => AppModelProcessing::LocalOnly,
        AppAgentProcessingClass::RemoteModel => AppModelProcessing::RemoteAllowed,
    }
}

struct AppDiscoveryProgressGuard<'a>(&'a Connection);

impl Drop for AppDiscoveryProgressGuard<'_> {
    fn drop(&mut self) {
        self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

fn install_discovery_progress_guard(connection: &Connection) -> AppDiscoveryProgressGuard<'_> {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(MAX_APP_DISCOVERY_SQL_MILLIS);
    let mut callbacks = 0usize;
    let progress_interval = usize::try_from(APP_DISCOVERY_PROGRESS_INTERVAL_OPS)
        .unwrap_or(1)
        .max(1);
    let maximum_callbacks = (MAX_APP_DISCOVERY_VM_STEPS / progress_interval).max(1);
    connection.progress_handler(
        APP_DISCOVERY_PROGRESS_INTERVAL_OPS,
        Some(move || {
            callbacks = callbacks.saturating_add(1);
            callbacks >= maximum_callbacks || std::time::Instant::now() >= deadline
        }),
    );
    AppDiscoveryProgressGuard(connection)
}

fn map_discovery_sqlite_error(error: rusqlite::Error) -> AppDiscoveryError {
    if matches!(
        &error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == ErrorCode::OperationInterrupted
    ) {
        AppDiscoveryError::CandidateScanLimitExceeded
    } else {
        AppDiscoveryError::Sqlite(error)
    }
}

fn list_discovery_snapshot(
    connection: &Connection,
    scope: &AppScope,
    audience: &AppStoreReadAudience,
    query: &AppDiscoveryListQuery,
) -> Result<AppDiscoveryPage, AppDiscoveryError> {
    let search = query.normalized_search();
    let search_digest = AppDigest::blake3(search.as_bytes());
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
    if cursor.as_ref().is_some_and(|cursor| {
        cursor.search_digest != search_digest || !cursor.sort_name.starts_with(&search)
    }) {
        return Err(AppDiscoveryError::InvalidCursor);
    }
    let prefix_end = discovery_prefix_upper_bound(&search);
    let limits = AppContractLimits::default();
    let _progress_guard = install_discovery_progress_guard(connection);
    let mut statement = connection
        .prepare(
            "SELECT length(i.record_json) + length(p.record_json)
                + length(m.metadata_json) + length(g.record_json)
                + length(s.record_json) + (2 * length(m.name)) AS raw_row_bytes,
                i.record_json, p.record_json, m.metadata_json, g.record_json,
                s.record_json, m.name, LOWER(m.name) AS sort_name
           FROM app_package_directory_metadata m
                INDEXED BY app_package_directory_metadata_discovery_idx
           CROSS JOIN app_installations i
                INDEXED BY app_installations_package_discovery_idx
           JOIN app_package_revisions p
             ON p.package_revision_ref = i.package_revision_ref
           JOIN app_grant_revisions g
             ON g.installation_id = i.installation_id
            AND g.revision = CAST(json_extract(i.record_json, '$.grant_revision') AS INTEGER)
            AND g.package_revision_ref = i.package_revision_ref
           JOIN app_schema_revisions s
             ON s.installation_id = i.installation_id
            AND s.revision = CAST(json_extract(i.record_json, '$.active_schema_revision') AS \
             INTEGER)
            AND s.package_revision_ref = i.package_revision_ref
          WHERE i.principal = ?1 AND i.workspace = ?2
            AND i.package_revision_ref = m.package_revision_ref
            AND i.lifecycle_status = 'enabled'
            AND length(i.record_json) BETWEEN 1 AND ?3
            AND length(p.record_json) BETWEEN 1 AND ?3
            AND length(m.metadata_json) BETWEEN 1 AND ?3
            AND length(m.name) BETWEEN 1 AND 64
            AND length(g.record_json) BETWEEN 1 AND ?3
            AND length(s.record_json) BETWEEN 1 AND ?3
            AND (LOWER(m.name), m.package_revision_ref) >= (?5, ?6)
            AND LOWER(m.name) < ?4
            AND (?7 IS NULL OR (
                  LOWER(m.name), m.package_revision_ref, i.installation_id
                ) > (?7, ?8, ?9))
          ORDER BY LOWER(m.name) ASC, m.package_revision_ref ASC,
                   i.installation_id ASC
          LIMIT ?10",
        )
        .map_err(map_discovery_sqlite_error)?;
    let cursor_sort = cursor.as_ref().map(|cursor| cursor.sort_name.as_str());
    let cursor_package = cursor
        .as_ref()
        .map(|cursor| cursor.package_revision_ref.as_str());
    let cursor_installation = cursor
        .as_ref()
        .map(|cursor| cursor.installation_id.as_str());
    let scan_sort = cursor_sort.unwrap_or(search.as_str());
    let scan_package = cursor_package.unwrap_or("");
    let mut rows = statement
        .query(params![
            scope.principal.as_str(),
            scope.workspace.as_str(),
            i64::try_from(limits.max_document_bytes())
                .map_err(|_| AppDiscoveryError::Corrupt("document limit overflow"))?,
            prefix_end,
            scan_sort,
            scan_package,
            cursor_sort,
            cursor_package,
            cursor_installation,
            i64::try_from(MAX_APP_DISCOVERY_CANDIDATE_SCAN.saturating_add(1))
                .map_err(|_| AppDiscoveryError::Corrupt("candidate scan limit overflowed"))?,
        ])
        .map_err(map_discovery_sqlite_error)?;
    let mut candidates = Vec::with_capacity(query.limit.saturating_add(1));
    let mut scanned_candidates = 0usize;
    let mut scanned_raw_bytes = 0usize;
    let mut scan_limit_reached = false;
    while let Some(row) = rows.next().map_err(map_discovery_sqlite_error)? {
        if scanned_candidates == MAX_APP_DISCOVERY_CANDIDATE_SCAN {
            scan_limit_reached = true;
            break;
        }
        scanned_candidates = scanned_candidates.saturating_add(1);
        charge_discovery_scan_bytes(&mut scanned_raw_bytes, row.get::<_, i64>(0)?)?;
        let (
            installation_json,
            package_json,
            metadata_json,
            grant_json,
            schema_json,
            metadata_name,
            sort_name,
        ) = (
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, Vec<u8>>(3)?,
            row.get::<_, Vec<u8>>(4)?,
            row.get::<_, Vec<u8>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        );
        let installation: AppInstallation = decode_app_contract(&installation_json, &limits)?;
        let package: AppPackageRevision = decode_app_contract(&package_json, &limits)?;
        let metadata: AppPackageDirectoryMetadata = decode_app_contract(&metadata_json, &limits)?;
        let grant: AppGrantRevision = decode_app_contract(&grant_json, &limits)?;
        let schema: AppSchemaRevision = decode_app_contract(&schema_json, &limits)?;
        let granted_policy_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(
            &grant.granted_data_handling_policy,
        )?)?;
        if installation.scope != *scope
            || installation.lifecycle.status != AppInstallationStatus::Enabled
            || installation.package_revision_ref
                != super::registry::canonical_package_revision_ref(&package)?
            || metadata.package_revision_ref != installation.package_revision_ref
            || metadata.name.as_str() != metadata_name
            || sort_name != metadata_name.to_lowercase()
            || grant.installation_id != installation.installation_id
            || Some(grant.revision) != installation.grant_revision
            || grant.package_revision_ref != installation.package_revision_ref
            || grant.granted_data_handling_policy_digest != granted_policy_digest
            || schema.installation_id != installation.installation_id
            || Some(schema.revision) != installation.active_schema_revision
            || schema.package_revision_ref != installation.package_revision_ref
            || schema.canonical_data_handling_policy != grant.granted_data_handling_policy
        {
            return Err(AppDiscoveryError::Corrupt(
                "directory row differs from scoped installation truth",
            ));
        }
        if grant.revoked_at.is_some()
            || !grant_policy_permits_audience(&grant.granted_data_handling_policy, audience)
        {
            continue;
        }
        candidates.push(AppDiscoveryCandidate {
            installation,
            package,
            metadata,
            sort_name,
        });
        if candidates.len() > query.limit {
            break;
        }
    }

    let has_more = candidates.len() > query.limit;
    if !has_more && scan_limit_reached {
        return Err(AppDiscoveryError::CandidateScanLimitExceeded);
    }
    candidates.truncate(query.limit);
    let next_cursor = if has_more {
        candidates
            .last()
            .map(|candidate| {
                encode_cursor(&AppDiscoveryCursor {
                    version: 2,
                    search_digest,
                    sort_name: candidate.sort_name.clone(),
                    package_revision_ref: candidate.installation.package_revision_ref.clone(),
                    installation_id: candidate.installation.installation_id.clone(),
                })
            })
            .transpose()?
    } else {
        None
    };
    let apps = candidates
        .into_iter()
        .map(|candidate| AppDiscoverySummary {
            installation_id: candidate.installation.installation_id,
            name: candidate.metadata.name,
            package_version: candidate.package.semantic_version,
        })
        .collect();
    Ok(AppDiscoveryPage {
        protocol_version: AppProtocolVersion::V1,
        apps,
        next_cursor,
        has_more,
    })
}

fn build_discovered_app(
    installation_id: &AppInstallationId,
    package: &AppPackageRevision,
    manifest: &AppPackageManifest,
    active: &ActiveAppEntitySchema,
    audience: &AppStoreReadAudience,
) -> Result<AppDiscoveredApp, AppDiscoveryError> {
    let mut entities = Vec::new();
    let globally_readable = active
        .grant()
        .granted_data_handling_policy
        .personal_agent_access
        == AppPersonalAgentAccess::ApprovedProjection;
    if globally_readable {
        for grant in &active.grant().granted_personal_agent_data_access {
            let runtime =
                active
                    .runtime_contract(&grant.entity)
                    .ok_or(AppDiscoveryError::Corrupt(
                        "projection grant references an unknown entity",
                    ))?;
            let mut fields = Vec::new();
            for field in &grant.fields {
                let contract = runtime.field(field).ok_or(AppDiscoveryError::Corrupt(
                    "projection grant references an unknown field",
                ))?;
                let policy = contract.effective_policy();
                if policy.personal_agent_access != AppPersonalAgentAccess::ApprovedProjection
                    || !audience
                        .permits_policy(policy.classification_floor, policy.model_processing)
                {
                    continue;
                }
                fields.push(AppDiscoveredField {
                    field: field.clone(),
                    field_type: contract.kind(),
                    required: contract.required(),
                    nullable: contract.nullable(),
                    enum_values: contract.enum_values().iter().cloned().collect(),
                });
            }
            fields.sort_by(|left, right| left.field.cmp(&right.field));
            if !fields.is_empty() {
                entities.push(AppDiscoveredEntity {
                    entity: grant.entity.clone(),
                    search: grant.search,
                    fields,
                });
            }
        }
    }
    entities.sort_by(|left, right| left.entity.cmp(&right.entity));

    let visible_fields = entities
        .iter()
        .map(|entity| {
            (
                entity.entity.clone(),
                entity
                    .fields
                    .iter()
                    .map(|field| field.field.clone())
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut views = Vec::new();
    for (view_id, view) in &manifest.app.views {
        let Some(allowed) = visible_fields.get(&view.entity) else {
            continue;
        };
        let projected_fields = discovered_view_fields(view)?
            .into_iter()
            .filter(|field| allowed.contains(field))
            .collect::<Vec<_>>();
        if projected_fields.is_empty() {
            continue;
        }
        views.push(AppDiscoveredView {
            view_id: view_id.clone(),
            entity: view.entity.clone(),
            view_type: view.kind,
            projected_fields,
        });
    }

    let mut actions = Vec::new();
    let manifest_default_policy = AppDataHandlingPolicy {
        classification_floor: manifest.app.data_policy.defaults.classification_floor,
        model_processing: manifest.app.data_policy.defaults.model_processing,
        personal_agent_access: manifest.app.data_policy.defaults.personal_agent_access,
        memory_promotion: manifest.app.data_policy.defaults.memory_promotion,
        external_egress: manifest.app.data_policy.defaults.external_egress,
        approved_destinations: manifest
            .app
            .data_policy
            .defaults
            .approved_destinations
            .clone(),
    };
    let action_base_policy = intersect_app_data_handling_policies(
        &active.grant().granted_data_handling_policy,
        &manifest_default_policy,
    );
    for (action_id, action) in &manifest.app.actions {
        let workflow =
            manifest
                .app
                .workflows
                .get(&action.workflow)
                .ok_or(AppDiscoveryError::Corrupt(
                    "action references an unknown workflow",
                ))?;
        if workflow.trigger != AppManifestTrigger::User
            || !declared_workflow_is_grant_eligible(active.grant(), workflow)
        {
            continue;
        }
        // Discovery cannot know which optional fields a later invocation will
        // contain. Expose an action only when its complete declared input
        // shape is safe for this physical caller. Direct invocation still
        // resolves and rechecks the exact value-dependent floor at launch.
        if !schema_permits_audience(&workflow.input, &action_base_policy, audience) {
            continue;
        }
        let result_schema = workflow
            .result
            .output_schema
            .as_ref()
            .filter(|schema| {
                globally_readable && schema_permits_audience(schema, &action_base_policy, audience)
            })
            .map(sanitize_object_schema)
            .transpose()?;
        let result_delivery = result_delivery_for_schema(result_schema.as_ref());
        actions.push(AppDiscoveredAction {
            action_id: action_id.clone(),
            input_schema: sanitize_object_schema(&workflow.input)?,
            result_schema,
            result_delivery,
        });
    }

    let app = AppDiscoveredApp {
        protocol_version: AppProtocolVersion::V1,
        installation_id: installation_id.clone(),
        name: manifest.name.clone(),
        package_version: package.semantic_version.clone(),
        entities,
        views,
        actions,
    };
    Ok(app)
}

fn discovered_view_fields(view: &AppManifestView) -> Result<Vec<AppFieldPath>, AppDiscoveryError> {
    let mut names = view.columns.clone();
    names.extend(
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
    let mut fields = names
        .into_iter()
        .map(|name| AppFieldPath::parse(name.as_str()).map_err(AppDiscoveryError::from))
        .collect::<Result<Vec<_>, _>>()?;
    fields.sort();
    fields.dedup();
    Ok(fields)
}

fn sanitize_object_schema(
    schema: &AppManifestInputSchema,
) -> Result<AppDiscoveredObjectSchema, AppDiscoveryError> {
    let fields = schema
        .fields
        .iter()
        .map(|(name, field)| {
            let (field_type, required, nullable, enum_values) = sanitize_manifest_field(field);
            AppDiscoveredSchemaField {
                field: name.clone(),
                field_type,
                required,
                nullable,
                enum_values,
            }
        })
        .collect();
    Ok(AppDiscoveredObjectSchema { fields })
}

fn result_delivery_for_schema(
    result_schema: Option<&AppDiscoveredObjectSchema>,
) -> AppDiscoveredResultDelivery {
    if result_schema.is_some() {
        AppDiscoveredResultDelivery::TypedWhenEligible
    } else {
        AppDiscoveredResultDelivery::RunHandleOnly
    }
}

fn schema_permits_audience(
    schema: &AppManifestInputSchema,
    base_policy: &AppDataHandlingPolicy,
    audience: &AppStoreReadAudience,
) -> bool {
    base_policy.personal_agent_access == AppPersonalAgentAccess::ApprovedProjection
        && audience.permits_policy(
            base_policy.classification_floor,
            base_policy.model_processing,
        )
        && schema.fields.values().all(|field| {
            let policy = restrict_policy(base_policy.clone(), field.policy());
            policy.personal_agent_access == AppPersonalAgentAccess::ApprovedProjection
                && audience.permits_policy(policy.classification_floor, policy.model_processing)
        })
}

fn sanitize_manifest_field(
    field: &AppManifestField,
) -> (AppQueryScalarKind, bool, bool, Vec<AppName>) {
    match field {
        AppManifestField::Text {
            required, nullable, ..
        } => (AppQueryScalarKind::Text, *required, *nullable, Vec::new()),
        AppManifestField::Markdown {
            required, nullable, ..
        } => (
            AppQueryScalarKind::Markdown,
            *required,
            *nullable,
            Vec::new(),
        ),
        AppManifestField::Integer {
            required, nullable, ..
        } => (
            AppQueryScalarKind::Integer,
            *required,
            *nullable,
            Vec::new(),
        ),
        AppManifestField::Decimal {
            required, nullable, ..
        } => (
            AppQueryScalarKind::Decimal,
            *required,
            *nullable,
            Vec::new(),
        ),
        AppManifestField::Boolean {
            required, nullable, ..
        } => (
            AppQueryScalarKind::Boolean,
            *required,
            *nullable,
            Vec::new(),
        ),
        AppManifestField::Timestamp {
            required, nullable, ..
        } => (
            AppQueryScalarKind::Timestamp,
            *required,
            *nullable,
            Vec::new(),
        ),
        AppManifestField::Enum {
            values,
            required,
            nullable,
            ..
        } => (
            AppQueryScalarKind::Enum,
            *required,
            *nullable,
            values.clone(),
        ),
        AppManifestField::Reference {
            required, nullable, ..
        } => (
            AppQueryScalarKind::Reference,
            *required,
            *nullable,
            Vec::new(),
        ),
    }
}

fn enforce_response_bound(value: &impl Serialize) -> Result<(), AppDiscoveryError> {
    let value = serde_json::to_value(value)?;
    if exact_json_encoded_len(&value) > MAX_APP_DISCOVERY_RESPONSE_BYTES {
        return Err(AppDiscoveryError::ResponseTooLarge);
    }
    Ok(())
}

fn prefix_successor(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let mut bytes = value.as_bytes().to_vec();
    for index in (0..bytes.len()).rev() {
        if bytes[index] != u8::MAX {
            bytes[index] = bytes[index].saturating_add(1);
            bytes.truncate(index.saturating_add(1));
            return String::from_utf8(bytes).ok();
        }
    }
    None
}

fn discovery_prefix_upper_bound(value: &str) -> String {
    prefix_successor(value).unwrap_or_else(|| APP_DISCOVERY_ALL_NAMES_UPPER_BOUND.to_owned())
}

fn charge_discovery_scan_bytes(
    scanned_raw_bytes: &mut usize,
    row_bytes: i64,
) -> Result<(), AppDiscoveryError> {
    let row_bytes =
        usize::try_from(row_bytes).map_err(|_| AppDiscoveryError::RawScanByteLimitExceeded)?;
    let next = scanned_raw_bytes
        .checked_add(row_bytes)
        .filter(|next| *next <= MAX_APP_DISCOVERY_RAW_SCAN_BYTES)
        .ok_or(AppDiscoveryError::RawScanByteLimitExceeded)?;
    *scanned_raw_bytes = next;
    Ok(())
}

fn encode_cursor(cursor: &AppDiscoveryCursor) -> Result<String, AppDiscoveryError> {
    let bytes = serde_json::to_vec(cursor)?;
    let encoded = URL_SAFE_NO_PAD.encode(bytes);
    if encoded.len() > MAX_APP_DISCOVERY_CURSOR_BYTES {
        return Err(AppDiscoveryError::Corrupt(
            "encoded discovery cursor exceeds its byte ceiling",
        ));
    }
    Ok(encoded)
}

fn decode_cursor(raw: &str) -> Result<AppDiscoveryCursor, AppDiscoveryError> {
    if raw.len() > MAX_APP_DISCOVERY_CURSOR_BYTES {
        return Err(AppDiscoveryError::InvalidCursor);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| AppDiscoveryError::InvalidCursor)?;
    if bytes.len() > MAX_APP_DISCOVERY_CURSOR_BYTES {
        return Err(AppDiscoveryError::InvalidCursor);
    }
    let cursor: AppDiscoveryCursor =
        serde_json::from_slice(&bytes).map_err(|_| AppDiscoveryError::InvalidCursor)?;
    if cursor.version != 2
        || cursor.sort_name.is_empty()
        || cursor.sort_name.len() > 64
        || AppName::parse(cursor.sort_name.clone()).is_err()
    {
        return Err(AppDiscoveryError::InvalidCursor);
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_query_rejects_unbounded_or_control_searches() {
        let overlong = AppDiscoveryListQuery {
            search: Some("x".repeat(MAX_APP_DISCOVERY_SEARCH_BYTES + 1)),
            cursor: None,
            limit: DEFAULT_APP_DISCOVERY_LIMIT,
        };
        assert!(matches!(
            overlong.validate(),
            Err(AppDiscoveryError::InvalidRequest(_))
        ));
        let control = AppDiscoveryListQuery {
            search: Some("hello\nworld".to_owned()),
            cursor: None,
            limit: DEFAULT_APP_DISCOVERY_LIMIT,
        };
        assert!(matches!(
            control.validate(),
            Err(AppDiscoveryError::InvalidRequest(_))
        ));
        let non_name_prefix = AppDiscoveryListQuery {
            search: Some("calendar event".to_owned()),
            cursor: None,
            limit: DEFAULT_APP_DISCOVERY_LIMIT,
        };
        assert!(matches!(
            non_name_prefix.validate(),
            Err(AppDiscoveryError::InvalidRequest(_))
        ));
        let whitespace_only = AppDiscoveryListQuery {
            search: Some("   ".to_owned()),
            cursor: None,
            limit: DEFAULT_APP_DISCOVERY_LIMIT,
        };
        assert!(matches!(
            whitespace_only.validate(),
            Err(AppDiscoveryError::InvalidRequest(_))
        ));
    }

    #[test]
    fn discovery_prefix_range_is_ascii_normalized_and_bounded() {
        let query = AppDiscoveryListQuery {
            search: Some("  Cal  ".to_owned()),
            cursor: None,
            limit: DEFAULT_APP_DISCOVERY_LIMIT,
        };
        query.validate().unwrap();
        assert_eq!(query.normalized_search(), "cal");
        assert_eq!(prefix_successor("cal").as_deref(), Some("cam"));
        assert_eq!(prefix_successor(""), None);
        assert_eq!(discovery_prefix_upper_bound("cal"), "cam");
        assert_eq!(
            discovery_prefix_upper_bound(""),
            APP_DISCOVERY_ALL_NAMES_UPPER_BOUND
        );
    }

    #[test]
    fn discovery_raw_scan_ceiling_is_aggregate_and_fail_closed() {
        let mut scanned = MAX_APP_DISCOVERY_RAW_SCAN_BYTES - 1;
        charge_discovery_scan_bytes(&mut scanned, 1).unwrap();
        assert_eq!(scanned, MAX_APP_DISCOVERY_RAW_SCAN_BYTES);
        assert!(matches!(
            charge_discovery_scan_bytes(&mut scanned, 1),
            Err(AppDiscoveryError::RawScanByteLimitExceeded)
        ));
        assert_eq!(scanned, MAX_APP_DISCOVERY_RAW_SCAN_BYTES);
        let mut invalid_total = 0;
        assert!(matches!(
            charge_discovery_scan_bytes(&mut invalid_total, -1),
            Err(AppDiscoveryError::RawScanByteLimitExceeded)
        ));
    }

    #[test]
    fn discovery_cursor_is_query_bound_and_round_trips() {
        let cursor = AppDiscoveryCursor {
            version: 2,
            search_digest: AppDigest::blake3(b"calendar"),
            sort_name: "calendar".to_owned(),
            package_revision_ref: super::super::models::AppReference::parse("package:calendar-v1")
                .unwrap(),
            installation_id: AppInstallationId::parse("installation-1").unwrap(),
        };
        let encoded = encode_cursor(&cursor).unwrap();
        let decoded = decode_cursor(&encoded).unwrap();
        assert_eq!(decoded.search_digest, cursor.search_digest);
        assert_eq!(decoded.sort_name, cursor.sort_name);
        assert_eq!(decoded.package_revision_ref, cursor.package_revision_ref);
        assert_eq!(decoded.installation_id, cursor.installation_id);
    }

    #[test]
    fn manifest_schema_sanitizer_omits_policy_and_preserves_shape() {
        let schema: AppManifestInputSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "fields": {
                "status": {
                    "type": "enum",
                    "values": ["open", "closed"],
                    "required": true,
                    "data_policy": {
                        "classification_floor": "secret",
                        "approved_destinations": ["app:private"]
                    }
                }
            }
        }))
        .unwrap();
        let sanitized = sanitize_object_schema(&schema).unwrap();
        let value = serde_json::to_value(sanitized).unwrap();
        assert_eq!(value["fields"][0]["field_type"], "enum");
        assert_eq!(value["fields"][0]["required"], true);
        assert!(value.to_string().find("data_policy").is_none());
        assert!(value.to_string().find("approved_destinations").is_none());
    }

    #[test]
    fn result_delivery_matches_schema_eligibility() {
        let schema = AppDiscoveredObjectSchema { fields: Vec::new() };
        assert_eq!(
            result_delivery_for_schema(Some(&schema)),
            AppDiscoveredResultDelivery::TypedWhenEligible
        );
        assert_eq!(
            result_delivery_for_schema(None),
            AppDiscoveredResultDelivery::RunHandleOnly
        );
    }

    #[test]
    fn action_input_discovery_requires_every_declared_field_to_be_safe() {
        let audience = AppStoreReadAudience::PersonalAgent {
            execution_ref: super::super::models::AppReference::parse("execution:local").unwrap(),
            processing_class: super::super::boundary::AppAgentProcessingClass::LocalModel,
            maximum_classification: AppDataClassification::Secret,
        };
        let base_policy = AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Sensitive,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion: super::super::records::AppMemoryPromotion::Denied,
            external_egress: super::super::records::AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        };
        let allowed: AppManifestInputSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "fields": {
                "title": {"type": "text"},
                "private_note": {
                    "type": "text",
                    "data_policy": {
                        "classification_floor": "secret",
                        "model_processing": "local_only"
                    }
                }
            }
        }))
        .unwrap();
        let denied: AppManifestInputSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "fields": {
                "offline_secret": {
                    "type": "text",
                    "data_policy": {"model_processing": "none"}
                }
            }
        }))
        .unwrap();

        assert!(schema_permits_audience(&allowed, &base_policy, &audience));
        assert!(!schema_permits_audience(&denied, &base_policy, &audience));
    }

    #[test]
    fn action_input_discovery_rejects_a_denied_base_policy() {
        let audience = AppStoreReadAudience::PersonalAgent {
            execution_ref: super::super::models::AppReference::parse("execution:local").unwrap(),
            processing_class: super::super::boundary::AppAgentProcessingClass::LocalModel,
            maximum_classification: AppDataClassification::Secret,
        };
        let schema: AppManifestInputSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "fields": {}
        }))
        .unwrap();
        let denied = AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Sensitive,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: super::super::records::AppMemoryPromotion::Denied,
            external_egress: super::super::records::AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        };

        assert!(!schema_permits_audience(&schema, &denied, &audience));
    }

    #[test]
    fn discovery_conceals_grants_outside_the_physical_audience() {
        let audience = AppStoreReadAudience::PersonalAgent {
            execution_ref: super::super::models::AppReference::parse("execution:local").unwrap(),
            processing_class: super::super::boundary::AppAgentProcessingClass::LocalModel,
            maximum_classification: AppDataClassification::Sensitive,
        };
        let mut policy = AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Sensitive,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
            memory_promotion: super::super::records::AppMemoryPromotion::Denied,
            external_egress: super::super::records::AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        };
        assert!(grant_policy_permits_audience(&policy, &audience));

        policy.classification_floor = AppDataClassification::Secret;
        assert!(!grant_policy_permits_audience(&policy, &audience));
        policy.classification_floor = AppDataClassification::Sensitive;
        policy.model_processing = AppModelProcessing::None;
        assert!(!grant_policy_permits_audience(&policy, &audience));
        policy.model_processing = AppModelProcessing::LocalOnly;
        policy.personal_agent_access = AppPersonalAgentAccess::Denied;
        assert!(!grant_policy_permits_audience(&policy, &audience));
    }
}
