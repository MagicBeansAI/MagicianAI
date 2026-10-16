//! Authoritative, metadata-only Apps directory and user placement.
//!
//! Directory reads are rebuilt from scoped installation/package truth on every
//! page. They never depend on events, Published Surfaces, Briefings or app
//! entity payloads. User activity and pins are small read models in the same
//! registry-owned SQLite transaction boundary.

use std::collections::{HashMap, HashSet};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension,
    TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    lifecycle::AppInstallationStatus,
    manifest::AppManifestNavigationEntry,
    models::{
        decode_app_contract, AppContractError, AppContractLimits, AppDigest, AppInstallationId,
        AppName, ValidateAppContract,
    },
    records::{
        AppBackgroundExecution, AppGrantRevision, AppInstallation, AppNetworkPolicy,
        AppPackageDirectoryMetadata, AppPackageRevision, AppScope,
    },
    registry::{
        canonical_package_revision_ref, format_timestamp, AppRegistryError, AppRegistryService,
    },
    surface_hydration::{
        load_active_surface_directory_views_blocking, AppSurfaceDirectoryView,
        AppSurfaceHydrationError,
    },
};

pub const DEFAULT_APP_DIRECTORY_LIMIT: usize = 24;
pub const MAX_APP_DIRECTORY_LIMIT: usize = 100;
const MAX_DIRECTORY_SEARCH_BYTES: usize = 128;
const MAX_DIRECTORY_CURSOR_BYTES: usize = 512;
const MAX_DIRECTORY_CURSOR_DECODED_BYTES: usize = 384;
const MAX_DIRECTORY_VIEWS_PER_APP: usize = 128;
const MAX_DIRECTORY_ACTIONS_PER_APP: usize = 256;
// Leaves transport-envelope headroom below the strict 4 MiB native-client cap.
const MAX_DIRECTORY_PAGE_ENTRY_BYTES: usize = 3 * 1_048_576;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppDirectorySection {
    Installed,
    Pinned,
    Recent,
    NeedsAttention,
    Disabled,
    Recovery,
}

impl AppDirectorySection {
    fn label(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Pinned => "pinned",
            Self::Recent => "recent",
            Self::NeedsAttention => "needs_attention",
            Self::Disabled => "disabled",
            Self::Recovery => "recovery",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirectoryQuery {
    pub section: AppDirectorySection,
    pub pinned_target_kind: Option<AppDirectoryTargetKind>,
    pub search: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
}

impl AppDirectoryQuery {
    pub fn validate(&self) -> Result<(), AppDirectoryError> {
        if self.limit == 0 || self.limit > MAX_APP_DIRECTORY_LIMIT {
            return Err(AppDirectoryError::InvalidQuery(
                "directory limit is outside the supported range",
            ));
        }
        if self.pinned_target_kind.is_some() && self.section != AppDirectorySection::Pinned {
            return Err(AppDirectoryError::InvalidQuery(
                "a pinned target kind is valid only for the pinned section",
            ));
        }
        if self.search.as_ref().is_some_and(|search| {
            search.len() > MAX_DIRECTORY_SEARCH_BYTES || search.chars().any(char::is_control)
        }) {
            return Err(AppDirectoryError::InvalidQuery(
                "directory search is too large or contains control characters",
            ));
        }
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.len() > MAX_DIRECTORY_CURSOR_BYTES)
        {
            return Err(AppDirectoryError::InvalidCursor);
        }
        Ok(())
    }

    fn normalized_search(&self) -> String {
        self.search
            .as_deref()
            .map(str::trim)
            .filter(|search| !search.is_empty())
            .unwrap_or_default()
            .to_lowercase()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectoryIcon {
    kind: &'static str,
    value: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectoryView {
    view_id: String,
    label: String,
    route: String,
    pinned: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectoryAction {
    action_id: String,
    label: String,
    pinned: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectoryPermissionSummary {
    granted_tools: u64,
    granted_context_reads: u64,
    granted_personal_data_projections: u64,
    background_execution: bool,
    network_access: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectoryStorageSummary {
    record_count: u64,
    revision_count: u64,
    payload_bytes: u64,
    attachment_bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectoryEntry {
    installation_id: String,
    name: String,
    description: String,
    icon: AppDirectoryIcon,
    package_version: String,
    package_revision_ref: String,
    installation_generation: u64,
    status: AppInstallationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_route: Option<String>,
    views: Vec<AppDirectoryView>,
    actions: Vec<AppDirectoryAction>,
    // Declared custom-surface entry points captured from the installed
    // package's manifest at admission. Clients gate custom-surface
    // affordances (the iOS viewless "Open app" button) on this hostability
    // signal; a package with nothing hostable reports 0.
    custom_surface_entry_count: usize,
    // First-party navigation the admitted manifest declared (gate S3), so the
    // shell can mount a system package's console without a second read. Only
    // an *enabled* installation contributes: a package parked for review,
    // disabled or quarantined is installed but not granted, and must not put a
    // link in first-party chrome. Absent means "declares none", which mounts
    // nothing — the same thing an older client that ignores the field does.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    navigation: Vec<AppManifestNavigationEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_opened_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attention_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permissions: Option<AppDirectoryPermissionSummary>,
    storage: AppDirectoryStorageSummary,
    // Kept flat for existing web/iOS consumers while `storage` is the durable
    // forward contract.
    record_count: u64,
    payload_bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectoryPage {
    entries: Vec<AppDirectoryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    has_more: bool,
}

impl AppDirectoryPage {
    fn empty() -> Self {
        Self {
            entries: Vec::new(),
            next_cursor: None,
            has_more: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppDirectoryTargetKind {
    View,
    Action,
}

impl AppDirectoryTargetKind {
    fn label(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Action => "action",
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppDirectoryActivityRequest {
    Opened {
        view_id: AppName,
    },
    Pin {
        target_kind: AppDirectoryTargetKind,
        target_id: AppName,
        pinned: bool,
    },
}

impl ValidateAppContract for AppDirectoryActivityRequest {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDirectoryActivityReceipt {
    installation_id: String,
    updated_at: String,
}

#[derive(Debug, Error)]
pub enum AppDirectoryError {
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Surface(#[from] AppSurfaceHydrationError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid Apps directory cursor")]
    InvalidCursor,
    #[error("invalid Apps directory query: {0}")]
    InvalidQuery(&'static str),
    #[error("the app installation is not available for directory activity")]
    InstallationUnavailable,
    #[error("the app directory target is not part of the active installation")]
    TargetUnavailable,
    #[error("corrupt Apps directory state: {0}")]
    Corrupt(&'static str),
}

#[derive(Debug, Clone)]
pub struct AppDirectoryService {
    registry: AppRegistryService,
}

impl AppDirectoryService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self { registry }
    }

    pub async fn list(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        query: AppDirectoryQuery,
        now: DateTime<Utc>,
    ) -> Result<AppDirectoryPage, AppDirectoryError> {
        query.validate()?;
        let page = self
            .registry
            .execute_scoped_typed_read(authenticated_scope, &now, move |connection, scope| {
                list_directory_blocking(connection, scope, &query)
            })
            .await?;
        Ok(page.unwrap_or_else(AppDirectoryPage::empty))
    }

    pub async fn record_activity(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        request: AppDirectoryActivityRequest,
        now: DateTime<Utc>,
    ) -> Result<AppDirectoryActivityReceipt, AppDirectoryError> {
        request.validate_app_contract(&AppContractLimits::default())?;
        let installation_id = installation_id.clone();
        let activity_time = now.clone();
        self.registry
            .execute_scoped_typed_write(authenticated_scope, &now, move |connection, scope| {
                record_activity_blocking(
                    connection,
                    scope,
                    &installation_id,
                    request,
                    &activity_time,
                )
            })
            .await
    }
}

#[derive(Debug)]
struct DirectoryCandidate {
    installation: AppInstallation,
    package: AppPackageRevision,
    metadata: Option<AppPackageDirectoryMetadata>,
    last_opened_at: Option<String>,
    attention_reason: Option<String>,
    storage: AppDirectoryStorageSummary,
    sort_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryCursor {
    version: u8,
    section: AppDirectorySection,
    pinned_target_kind: Option<AppDirectoryTargetKind>,
    search_digest: AppDigest,
    sort_at: String,
    installation_id: AppInstallationId,
}

fn list_directory_blocking(
    connection: &Connection,
    scope: &AppScope,
    query: &AppDirectoryQuery,
) -> Result<AppDirectoryPage, AppDirectoryError> {
    let transaction = connection.unchecked_transaction()?;
    let page = list_directory_snapshot(&transaction, scope, query)?;
    transaction.commit()?;
    Ok(page)
}

fn list_directory_snapshot(
    connection: &Connection,
    scope: &AppScope,
    query: &AppDirectoryQuery,
) -> Result<AppDirectoryPage, AppDirectoryError> {
    let search = query.normalized_search();
    let search_digest = AppDigest::blake3(search.as_bytes());
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
    if cursor.as_ref().is_some_and(|cursor| {
        cursor.section != query.section
            || cursor.pinned_target_kind != query.pinned_target_kind
            || cursor.search_digest != search_digest
    }) {
        return Err(AppDirectoryError::InvalidCursor);
    }
    let search_pattern = format!("%{}%", escape_like_pattern(&search));
    let cursor_sort = cursor.as_ref().map(|cursor| cursor.sort_at.as_str());
    let cursor_installation = cursor
        .as_ref()
        .map(|cursor| cursor.installation_id.as_str());
    let fetch_limit = query
        .limit
        .checked_add(1)
        .ok_or(AppDirectoryError::InvalidQuery(
            "directory limit overflowed",
        ))?;
    let fetch_limit = i64::try_from(fetch_limit)
        .map_err(|_| AppDirectoryError::InvalidQuery("directory limit overflowed"))?;

    let mut candidates = {
        let mut statement = connection.prepare(
            "WITH candidates AS (
                 SELECT i.installation_id, i.lifecycle_status,
                        i.record_json AS installation_json,
                        p.record_json AS package_json, m.metadata_json,
                        m.name, m.description, m.created_at, s.last_opened_at,
                        COALESCE(u.record_count, 0) AS record_count,
                        COALESCE(u.revision_count, 0) AS revision_count,
                        COALESCE(u.payload_bytes, 0) AS payload_bytes,
                        COALESCE(u.attachment_bytes, 0) AS attachment_bytes,
                        (SELECT json_extract(a.record_json, '$.failure_code')
                           FROM app_lifecycle_attempts a
                          WHERE a.installation_id = i.installation_id AND a.state = 'failed'
                          ORDER BY a.updated_at DESC, a.attempt_id DESC LIMIT 1) AS failure_code,
                        CASE ?1
                          WHEN 'recent' THEN COALESCE(s.last_opened_at, '')
                          WHEN 'pinned' THEN COALESCE((
                            SELECT MAX(dp.pinned_at) FROM app_directory_pins dp
                             WHERE dp.installation_id = i.installation_id
                          ), '')
                          ELSE i.updated_at
                        END AS sort_at
                   FROM app_installations i
                   JOIN app_package_revisions p
                     ON p.package_revision_ref = i.package_revision_ref
                   LEFT JOIN app_package_directory_metadata m
                     ON m.package_revision_ref = i.package_revision_ref
                   LEFT JOIN app_directory_state s
                     ON s.installation_id = i.installation_id
                   LEFT JOIN app_storage_usage u
                     ON u.installation_id = i.installation_id
                  WHERE i.principal = ?2 AND i.workspace = ?3
                    AND i.lifecycle_status != 'purged'
                    AND length(i.installation_id) BETWEEN 1 AND ?9
                    AND length(i.lifecycle_status) BETWEEN 1 AND ?10
                    AND length(i.record_json) BETWEEN 1 AND ?11
                    AND length(p.record_json) BETWEEN 1 AND ?11
                    AND length(i.updated_at) BETWEEN 1 AND ?12
                    AND (m.metadata_json IS NULL OR length(m.metadata_json) BETWEEN 1 AND ?11)
                    AND (m.name IS NULL OR length(m.name) BETWEEN 1 AND ?13)
                    AND (m.description IS NULL OR length(m.description) <= ?14)
                    AND (m.created_at IS NULL OR length(m.created_at) BETWEEN 1 AND ?12)
                    AND (s.last_opened_at IS NULL OR length(s.last_opened_at) BETWEEN 1 AND ?12)
                    AND (?4 = '%%' OR LOWER(COALESCE(m.name, p.package_id)) LIKE ?4 ESCAPE '\\'
                                      OR LOWER(COALESCE(m.description, '')) LIKE ?4 ESCAPE '\\')
                    AND CASE ?1
                      WHEN 'installed' THEN i.lifecycle_status = 'enabled'
                      WHEN 'recent' THEN i.lifecycle_status = 'enabled' AND s.last_opened_at IS \
             NOT NULL
                      WHEN 'pinned' THEN i.lifecycle_status = 'enabled' AND EXISTS (
                        SELECT 1 FROM app_directory_pins dp
                         WHERE dp.installation_id = i.installation_id
                           AND (?5 IS NULL OR dp.target_kind = ?5)
                           AND ((dp.target_kind = 'view' AND EXISTS (
                               SELECT 1 FROM app_surface_generations g
                               JOIN app_surface_generation_members gm
                                 ON gm.installation_id = g.installation_id
                                AND gm.revision = g.revision
                              WHERE g.installation_id = i.installation_id
                                AND g.revision = CAST(
                                  json_extract(i.record_json, '$.active_surface_revision') AS \
             INTEGER
                                )
                                AND gm.view_id = dp.target_id
                           )) OR (dp.target_kind = 'action' AND EXISTS (
                               SELECT 1 FROM app_package_directory_actions da
                                WHERE da.package_revision_ref = i.package_revision_ref
                                  AND da.action_id = dp.target_id
                           )))
                      )
                      WHEN 'needs_attention' THEN i.lifecycle_status IN (
                        'ready_for_review', 'update_pending', 'quarantined'
                      )
                      WHEN 'disabled' THEN i.lifecycle_status = 'disabled'
                      WHEN 'recovery' THEN i.lifecycle_status = 'uninstalled_retained'
                      ELSE 0
                    END
             )
             SELECT installation_id, lifecycle_status, installation_json, package_json, \
             metadata_json,
                    name, description, created_at, last_opened_at,
                    record_count, revision_count, payload_bytes,
                    attachment_bytes, failure_code, sort_at
               FROM candidates
              WHERE (?6 IS NULL OR sort_at < ?6
                    OR (sort_at = ?6 AND installation_id < ?7))
              ORDER BY sort_at DESC, installation_id DESC
              LIMIT ?8",
        )?;
        let rows = statement.query_map(
            params![
                query.section.label(),
                scope.principal.as_str(),
                scope.workspace.as_str(),
                search_pattern,
                query.pinned_target_kind.map(AppDirectoryTargetKind::label),
                cursor_sort,
                cursor_installation,
                fetch_limit,
                128_i64,
                32_i64,
                i64::try_from(AppContractLimits::default().max_document_bytes())
                    .map_err(|_| AppDirectoryError::Corrupt("document limit overflow"))?,
                64_i64,
                64_i64,
                2_048_i64,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, String>(14)?,
                ))
            },
        )?;
        let limits = AppContractLimits::default();
        let mut decoded = Vec::with_capacity(query.limit.saturating_add(1));
        for row in rows {
            let (
                installation_id,
                lifecycle_status,
                installation_json,
                package_json,
                metadata_json,
                metadata_name,
                metadata_description,
                metadata_created_at,
                last_opened_at,
                record_count,
                revision_count,
                payload_bytes,
                attachment_bytes,
                failure_code,
                sort_at,
            ) = row?;
            if failure_code.as_ref().is_some_and(|code| {
                code.is_empty() || code.len() > 128 || code.chars().any(char::is_control)
            }) || sort_at.len() > 64
                || sort_at.chars().any(char::is_control)
            {
                return Err(AppDirectoryError::Corrupt(
                    "directory ordering or failure metadata is invalid",
                ));
            }
            let installation: AppInstallation = decode_app_contract(&installation_json, &limits)?;
            let package: AppPackageRevision = decode_app_contract(&package_json, &limits)?;
            let package_revision_ref = canonical_package_revision_ref(&package)?;
            let metadata: Option<AppPackageDirectoryMetadata> = metadata_json
                .as_deref()
                .map(|bytes| decode_app_contract(bytes, &limits))
                .transpose()?;
            let metadata_matches_columns = match (
                metadata.as_ref(),
                metadata_name.as_deref(),
                metadata_description.as_deref(),
                metadata_created_at.as_deref(),
            ) {
                (None, None, None, None) => true,
                (Some(metadata), Some(name), Some(description), Some(created_at)) => {
                    metadata.name.as_str() == name
                        && metadata.description == description
                        && format_timestamp(&metadata.created_at) == created_at
                },
                _ => false,
            };
            if installation.installation_id.as_str() != installation_id
                || &installation.scope != scope
                || status_label(installation.lifecycle.status) != lifecycle_status
                || installation.package_revision_ref != package_revision_ref
                || metadata.as_ref().is_some_and(|metadata| {
                    metadata.package_revision_ref != installation.package_revision_ref
                })
                || !metadata_matches_columns
            {
                return Err(AppDirectoryError::Corrupt(
                    "directory rows do not match their canonical identities",
                ));
            }
            let attention_reason =
                attention_reason(installation.lifecycle.status, failure_code.as_deref());
            decoded.push(DirectoryCandidate {
                installation,
                package,
                metadata,
                last_opened_at,
                attention_reason,
                storage: AppDirectoryStorageSummary {
                    record_count: nonnegative_u64(record_count)?,
                    revision_count: nonnegative_u64(revision_count)?,
                    payload_bytes: nonnegative_u64(payload_bytes)?,
                    attachment_bytes: nonnegative_u64(attachment_bytes)?,
                },
                sort_at,
            });
        }
        decoded
    };

    let source_has_more = candidates.len() > query.limit;
    candidates.truncate(query.limit);
    let enabled_installations = candidates
        .iter()
        .filter(|candidate| {
            candidate.installation.lifecycle.status == AppInstallationStatus::Enabled
        })
        .map(|candidate| candidate.installation.clone())
        .collect::<Vec<_>>();
    let mut views_by_installation =
        load_active_surface_directory_views_blocking(connection, scope, &enabled_installations)?;
    let installation_ids = candidates
        .iter()
        .map(|candidate| candidate.installation.installation_id.clone())
        .collect::<Vec<_>>();
    let package_revision_refs = candidates
        .iter()
        .map(|candidate| candidate.installation.package_revision_ref.clone())
        .collect::<Vec<_>>();
    let pins_by_installation = load_pins_for_installations(connection, &installation_ids)?;
    let actions_by_package =
        load_directory_actions_for_packages(connection, &package_revision_refs)?;
    let mut permissions_by_installation = load_permission_summaries(connection, &candidates)?;
    let mut entries = Vec::with_capacity(candidates.len());
    let candidate_count = candidates.len();
    let mut entry_bytes = 0usize;
    let mut cursor_anchor = None;
    for candidate in candidates {
        let anchor = (
            candidate.sort_at.clone(),
            candidate.installation.installation_id.clone(),
        );
        let installation_id = candidate.installation.installation_id.clone();
        let package_revision_ref = candidate.installation.package_revision_ref.clone();
        let entry = build_entry(
            candidate,
            views_by_installation
                .remove(&installation_id)
                .unwrap_or_default(),
            pins_by_installation.get(&installation_id),
            actions_by_package
                .get(package_revision_ref.as_str())
                .map(Vec::as_slice),
            permissions_by_installation
                .remove(&installation_id)
                .flatten(),
        )?;
        let serialized_len = serde_json::to_vec(&entry)
            .map_err(|_| AppDirectoryError::Corrupt("directory entry could not be encoded"))?
            .len();
        let Some(next_bytes) = next_directory_page_bytes(entry_bytes, serialized_len)? else {
            break;
        };
        entry_bytes = next_bytes;
        cursor_anchor = Some(anchor);
        entries.push(entry);
    }
    let has_more = source_has_more || entries.len() < candidate_count;
    let next_cursor = if has_more {
        let (sort_at, installation_id) = cursor_anchor.ok_or(AppDirectoryError::Corrupt(
            "one directory entry exceeds the response byte ceiling",
        ))?;
        Some(encode_cursor(&DirectoryCursor {
            version: 1,
            section: query.section,
            pinned_target_kind: query.pinned_target_kind,
            search_digest,
            sort_at,
            installation_id,
        })?)
    } else {
        None
    };
    Ok(AppDirectoryPage {
        entries,
        next_cursor,
        has_more,
    })
}

fn next_directory_page_bytes(
    consumed: usize,
    serialized_entry: usize,
) -> Result<Option<usize>, AppDirectoryError> {
    // Account for the comma separating entries. The fixed envelope remains
    // outside this 3 MiB budget and fits inside the clients' remaining 1 MiB.
    let separator = usize::from(consumed != 0);
    let next = consumed
        .checked_add(separator)
        .and_then(|value| value.checked_add(serialized_entry))
        .ok_or(AppDirectoryError::Corrupt(
            "directory response size overflow",
        ))?;
    Ok((next <= MAX_DIRECTORY_PAGE_ENTRY_BYTES).then_some(next))
}

fn build_entry(
    candidate: DirectoryCandidate,
    directory_views: Vec<AppSurfaceDirectoryView>,
    pins: Option<&HashSet<(String, String)>>,
    durable_actions: Option<&[AppName]>,
    permissions: Option<AppDirectoryPermissionSummary>,
) -> Result<AppDirectoryEntry, AppDirectoryError> {
    let installation_id = &candidate.installation.installation_id;
    let empty_pins = HashSet::new();
    let pins = pins.unwrap_or(&empty_pins);
    let mut views = Vec::new();
    if candidate.installation.lifecycle.status == AppInstallationStatus::Enabled {
        if directory_views.len() > MAX_DIRECTORY_VIEWS_PER_APP {
            return Err(AppDirectoryError::Corrupt(
                "active surface generation exceeds the directory view bound",
            ));
        }
        for view in directory_views {
            if view
                .canonical_host_route
                .split('/')
                .any(|part| part.starts_with(':'))
            {
                continue;
            }
            let view_id = view.view_id.to_string();
            views.push(AppDirectoryView {
                label: humanize_identifier(&view_id),
                route: view.canonical_host_route,
                pinned: pins.contains(&("view".to_owned(), view_id.clone())),
                view_id,
            });
        }
        views.sort_by(|left, right| left.view_id.cmp(&right.view_id));
    }
    let actions = candidate
        .metadata
        .as_ref()
        .map(|metadata| {
            metadata
                .actions
                .iter()
                .map(|action| {
                    let action_id = action.to_string();
                    AppDirectoryAction {
                        label: humanize_identifier(&action_id),
                        pinned: pins.contains(&("action".to_owned(), action_id.clone())),
                        action_id,
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if actions.len() > MAX_DIRECTORY_ACTIONS_PER_APP {
        return Err(AppDirectoryError::Corrupt(
            "package metadata exceeds the directory action bound",
        ));
    }
    let durable_actions = durable_actions
        .map(|actions| actions.to_vec())
        .unwrap_or_default();
    let metadata_actions = candidate
        .metadata
        .as_ref()
        .map(|metadata| metadata.actions.clone())
        .unwrap_or_default();
    if durable_actions != metadata_actions {
        return Err(AppDirectoryError::Corrupt(
            "directory action index differs from package metadata",
        ));
    }
    let name = candidate.metadata.as_ref().map_or_else(
        || candidate.package.package_id.to_string(),
        |metadata| metadata.name.to_string(),
    );
    let description = candidate
        .metadata
        .as_ref()
        .map_or_else(String::new, |metadata| metadata.description.clone());
    let icon_value = name
        .chars()
        .find(|character| character.is_alphanumeric())
        .map(|character| character.to_uppercase().collect())
        .unwrap_or_else(|| "A".to_owned());
    let default_route = views
        .iter()
        .find(|view| view.route == format!("/apps/{installation_id}"))
        .or_else(|| views.first())
        .map(|view| view.route.clone());
    // Gated on `Enabled` for the same reason `views` above is: the directory
    // lists parked, disabled and quarantined installations too, and a
    // declaration a package made is not a grant the owner gave.
    let navigation = if candidate.installation.lifecycle.status == AppInstallationStatus::Enabled {
        candidate
            .metadata
            .as_ref()
            .map(|metadata| metadata.navigation.clone())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(AppDirectoryEntry {
        installation_id: installation_id.to_string(),
        name,
        description,
        icon: AppDirectoryIcon {
            kind: "monogram",
            value: icon_value,
        },
        package_version: candidate.package.semantic_version,
        package_revision_ref: candidate.installation.package_revision_ref.to_string(),
        installation_generation: candidate.installation.lifecycle.generation,
        status: candidate.installation.lifecycle.status,
        default_route,
        views,
        actions,
        custom_surface_entry_count: candidate
            .metadata
            .as_ref()
            .map_or(0, |metadata| metadata.custom_surface_entry_count),
        navigation,
        last_opened_at: candidate.last_opened_at,
        attention_reason: candidate.attention_reason,
        permissions,
        record_count: candidate.storage.record_count,
        payload_bytes: candidate.storage.payload_bytes,
        storage: candidate.storage,
    })
}

fn record_activity_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    request: AppDirectoryActivityRequest,
    now: &DateTime<Utc>,
) -> Result<AppDirectoryActivityReceipt, AppDirectoryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let installation_json: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let installation: AppInstallation = decode_app_contract(
        installation_json
            .as_deref()
            .ok_or(AppDirectoryError::InstallationUnavailable)?,
        &AppContractLimits::default(),
    )?;
    if &installation.scope != scope
        || installation.lifecycle.status != AppInstallationStatus::Enabled
    {
        return Err(AppDirectoryError::InstallationUnavailable);
    }
    match request {
        AppDirectoryActivityRequest::Opened { view_id } => {
            ensure_current_target(
                &transaction,
                &installation,
                AppDirectoryTargetKind::View,
                &view_id,
            )?;
            transaction.execute(
                "INSERT INTO app_directory_state (
                     installation_id, last_opened_at, last_view_id, updated_at
                 ) VALUES (?1, ?2, ?3, ?2)
                 ON CONFLICT(installation_id) DO UPDATE SET
                    last_opened_at = excluded.last_opened_at,
                    last_view_id = excluded.last_view_id,
                    updated_at = excluded.updated_at",
                params![
                    installation_id.as_str(),
                    format_timestamp(now),
                    view_id.as_str()
                ],
            )?;
        },
        AppDirectoryActivityRequest::Pin {
            target_kind,
            target_id,
            pinned,
        } => {
            ensure_current_target(&transaction, &installation, target_kind, &target_id)?;
            if pinned {
                transaction.execute(
                    "INSERT INTO app_directory_pins (
                         installation_id, target_kind, target_id, pinned_at
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(installation_id, target_kind, target_id)
                     DO UPDATE SET pinned_at = excluded.pinned_at",
                    params![
                        installation_id.as_str(),
                        target_kind.label(),
                        target_id.as_str(),
                        format_timestamp(now),
                    ],
                )?;
            } else {
                transaction.execute(
                    "DELETE FROM app_directory_pins
                      WHERE installation_id = ?1 AND target_kind = ?2 AND target_id = ?3",
                    params![
                        installation_id.as_str(),
                        target_kind.label(),
                        target_id.as_str()
                    ],
                )?;
            }
        },
    }
    transaction.commit()?;
    Ok(AppDirectoryActivityReceipt {
        installation_id: installation_id.to_string(),
        updated_at: format_timestamp(now),
    })
}

fn ensure_current_target(
    transaction: &rusqlite::Transaction<'_>,
    installation: &AppInstallation,
    target_kind: AppDirectoryTargetKind,
    target_id: &AppName,
) -> Result<(), AppDirectoryError> {
    let exists = match target_kind {
        AppDirectoryTargetKind::View => {
            let Some(surface_revision) = installation.active_surface_revision else {
                return Err(AppDirectoryError::TargetUnavailable);
            };
            transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM app_surface_generations g
                    JOIN app_surface_generation_members gm
                      ON gm.installation_id = g.installation_id AND gm.revision = g.revision
                   WHERE g.installation_id = ?1 AND g.revision = ?2
                     AND gm.view_id = ?3
                     AND instr(gm.canonical_host_route, '/:') = 0
                )",
                params![
                    installation.installation_id.as_str(),
                    i64::try_from(surface_revision.get())
                        .map_err(|_| AppDirectoryError::Corrupt("surface revision overflow"))?,
                    target_id.as_str(),
                ],
                |row| row.get::<_, bool>(0),
            )?
        },
        AppDirectoryTargetKind::Action => transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM app_package_directory_actions
                 WHERE package_revision_ref = ?1 AND action_id = ?2
            )",
            params![
                installation.package_revision_ref.as_str(),
                target_id.as_str()
            ],
            |row| row.get::<_, bool>(0),
        )?,
    };
    if !exists {
        return Err(AppDirectoryError::TargetUnavailable);
    }
    Ok(())
}

fn load_pins_for_installations(
    connection: &Connection,
    installation_ids: &[AppInstallationId],
) -> Result<HashMap<AppInstallationId, HashSet<(String, String)>>, AppDirectoryError> {
    let mut result = installation_ids
        .iter()
        .cloned()
        .map(|installation_id| (installation_id, HashSet::new()))
        .collect::<HashMap<_, _>>();
    if installation_ids.is_empty() {
        return Ok(result);
    }
    let placeholders = std::iter::repeat("?")
        .take(installation_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT installation_id, target_kind, target_id
           FROM app_directory_pins
          WHERE installation_id IN ({placeholders})
          ORDER BY installation_id ASC, target_kind ASC, target_id ASC",
    );
    let parameters = installation_ids
        .iter()
        .map(|installation_id| SqlValue::Text(installation_id.to_string()))
        .collect::<Vec<_>>();
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(parameters), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let max_pins = installation_ids
        .len()
        .checked_mul(MAX_DIRECTORY_VIEWS_PER_APP + MAX_DIRECTORY_ACTIONS_PER_APP)
        .ok_or(AppDirectoryError::Corrupt("directory pin bound overflow"))?;
    let mut count = 0usize;
    for row in rows {
        count = count
            .checked_add(1)
            .ok_or(AppDirectoryError::Corrupt("directory pin count overflow"))?;
        if count > max_pins {
            return Err(AppDirectoryError::Corrupt(
                "directory pins exceed the bounded page limit",
            ));
        }
        let (installation_id, target_kind, target_id) = row?;
        let installation_id = AppInstallationId::parse(installation_id)?;
        if !matches!(target_kind.as_str(), "view" | "action") {
            return Err(AppDirectoryError::Corrupt("directory pin kind is invalid"));
        }
        AppName::parse(target_id.clone())?;
        result
            .get_mut(&installation_id)
            .ok_or(AppDirectoryError::Corrupt(
                "directory pin belongs to an unexpected installation",
            ))?
            .insert((target_kind, target_id));
    }
    Ok(result)
}

fn load_directory_actions_for_packages(
    connection: &Connection,
    package_revision_refs: &[super::models::AppReference],
) -> Result<HashMap<String, Vec<AppName>>, AppDirectoryError> {
    let mut result = package_revision_refs
        .iter()
        .map(|reference| (reference.to_string(), Vec::new()))
        .collect::<HashMap<_, _>>();
    if package_revision_refs.is_empty() {
        return Ok(result);
    }
    let placeholders = std::iter::repeat("?")
        .take(package_revision_refs.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT package_revision_ref, action_id
           FROM app_package_directory_actions
          WHERE package_revision_ref IN ({placeholders})
          ORDER BY package_revision_ref ASC, action_id ASC",
    );
    let parameters = package_revision_refs
        .iter()
        .map(|reference| SqlValue::Text(reference.to_string()))
        .collect::<Vec<_>>();
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(parameters), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let max_actions = package_revision_refs
        .len()
        .checked_mul(MAX_DIRECTORY_ACTIONS_PER_APP)
        .ok_or(AppDirectoryError::Corrupt(
            "directory action bound overflow",
        ))?;
    let mut count = 0usize;
    for row in rows {
        count = count.checked_add(1).ok_or(AppDirectoryError::Corrupt(
            "directory action count overflow",
        ))?;
        if count > max_actions {
            return Err(AppDirectoryError::Corrupt(
                "directory actions exceed the bounded page limit",
            ));
        }
        let (package_revision_ref, action_id) = row?;
        let action = AppName::parse(action_id)?;
        result
            .get_mut(&package_revision_ref)
            .ok_or(AppDirectoryError::Corrupt(
                "directory action belongs to an unexpected package",
            ))?
            .push(action);
    }
    Ok(result)
}

fn load_permission_summaries(
    connection: &Connection,
    candidates: &[DirectoryCandidate],
) -> Result<HashMap<AppInstallationId, Option<AppDirectoryPermissionSummary>>, AppDirectoryError> {
    let mut result = candidates
        .iter()
        .map(|candidate| (candidate.installation.installation_id.clone(), None))
        .collect::<HashMap<_, _>>();
    let expected = candidates
        .iter()
        .filter(|candidate| candidate.installation.grant_revision.is_some())
        .map(|candidate| {
            (
                candidate.installation.installation_id.clone(),
                &candidate.installation,
            )
        })
        .collect::<HashMap<_, _>>();
    if expected.is_empty() {
        return Ok(result);
    }
    let mut clauses = Vec::with_capacity(expected.len());
    let mut parameters = Vec::with_capacity(expected.len().saturating_mul(2).saturating_add(1));
    for installation in expected.values() {
        let revision = installation
            .grant_revision
            .ok_or(AppDirectoryError::Corrupt("active grant is missing"))?;
        clauses.push("(installation_id = ? AND revision = ?)".to_owned());
        parameters.push(SqlValue::Text(installation.installation_id.to_string()));
        parameters
            .push(SqlValue::Integer(i64::try_from(revision.get()).map_err(
                |_| AppDirectoryError::Corrupt("grant revision overflow"),
            )?));
    }
    parameters.push(SqlValue::Integer(
        i64::try_from(AppContractLimits::default().max_document_bytes())
            .map_err(|_| AppDirectoryError::Corrupt("document limit overflow"))?,
    ));
    let document_limit = parameters.len();
    let sql = format!(
        "SELECT installation_id, revision, record_json
           FROM app_grant_revisions
          WHERE ({}) AND length(record_json) BETWEEN 1 AND ?{document_limit}
          ORDER BY installation_id ASC",
        clauses.join(" OR "),
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(parameters), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut loaded = HashSet::with_capacity(expected.len());
    for row in rows {
        let (installation_id, revision, bytes) = row?;
        let installation_id = AppInstallationId::parse(installation_id)?;
        let installation = expected
            .get(&installation_id)
            .ok_or(AppDirectoryError::Corrupt(
                "grant belongs to an unexpected directory installation",
            ))?;
        let expected_revision = installation
            .grant_revision
            .ok_or(AppDirectoryError::Corrupt("active grant is missing"))?;
        if revision
            != i64::try_from(expected_revision.get())
                .map_err(|_| AppDirectoryError::Corrupt("grant revision overflow"))?
            || !loaded.insert(installation_id.clone())
        {
            return Err(AppDirectoryError::Corrupt(
                "active directory grant identity is duplicated or mismatched",
            ));
        }
        let grant: AppGrantRevision = decode_app_contract(&bytes, &AppContractLimits::default())?;
        result.insert(
            installation_id,
            permission_summary_from_grant(installation, &grant)?,
        );
    }
    if loaded.len() != expected.len() {
        return Err(AppDirectoryError::Corrupt("active grant is missing"));
    }
    Ok(result)
}

fn permission_summary_from_grant(
    installation: &AppInstallation,
    grant: &AppGrantRevision,
) -> Result<Option<AppDirectoryPermissionSummary>, AppDirectoryError> {
    let grant_revision = installation
        .grant_revision
        .ok_or(AppDirectoryError::Corrupt("active grant is missing"))?;
    if grant.installation_id != installation.installation_id
        || grant.revision != grant_revision
        || grant.package_revision_ref != installation.package_revision_ref
    {
        return Err(AppDirectoryError::Corrupt(
            "active grant does not match the installation",
        ));
    }
    if grant.revoked_at.is_some() {
        if installation.lifecycle.status == AppInstallationStatus::Enabled {
            return Err(AppDirectoryError::Corrupt(
                "enabled installation references a revoked grant",
            ));
        }
        return Ok(None);
    }
    Ok(Some(AppDirectoryPermissionSummary {
        granted_tools: u64::try_from(grant.granted_tools.len())
            .map_err(|_| AppDirectoryError::Corrupt("tool count overflow"))?,
        granted_context_reads: u64::try_from(grant.granted_context_reads.len())
            .map_err(|_| AppDirectoryError::Corrupt("context-read count overflow"))?,
        granted_personal_data_projections: u64::try_from(
            grant.granted_personal_agent_data_access.len(),
        )
        .map_err(|_| AppDirectoryError::Corrupt("projection count overflow"))?,
        background_execution: matches!(
            grant.granted_background_execution,
            AppBackgroundExecution::Granted { .. }
        ),
        network_access: matches!(
            grant.granted_network_policy,
            AppNetworkPolicy::ApprovedDestinations { .. }
        ),
    }))
}

fn attention_reason(status: AppInstallationStatus, failure_code: Option<&str>) -> Option<String> {
    let status_reason = match status {
        AppInstallationStatus::ReadyForReview => Some("Approval required"),
        AppInstallationStatus::UpdatePending => Some("Update awaiting review"),
        AppInstallationStatus::Quarantined => Some("Quarantined for review"),
        _ => None,
    };
    failure_code
        .map(|code| format!("Last attempt failed: {}", humanize_identifier(code)))
        .or_else(|| status_reason.map(str::to_owned))
}

fn status_label(status: AppInstallationStatus) -> &'static str {
    match status {
        AppInstallationStatus::ReadyForReview => "ready_for_review",
        AppInstallationStatus::Enabled => "enabled",
        AppInstallationStatus::Disabled => "disabled",
        AppInstallationStatus::UpdatePending => "update_pending",
        AppInstallationStatus::Quarantined => "quarantined",
        AppInstallationStatus::UninstalledRetained => "uninstalled_retained",
        AppInstallationStatus::Purged => "purged",
    }
}

fn humanize_identifier(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut capitalize = true;
    for character in value.chars() {
        if matches!(character, '_' | '-') {
            if !result.ends_with(' ') {
                result.push(' ');
            }
            capitalize = true;
        } else if capitalize {
            result.extend(character.to_uppercase());
            capitalize = false;
        } else {
            result.push(character);
        }
    }
    result
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn nonnegative_u64(value: i64) -> Result<u64, AppDirectoryError> {
    u64::try_from(value).map_err(|_| AppDirectoryError::Corrupt("negative usage counter"))
}

fn encode_cursor(cursor: &DirectoryCursor) -> Result<String, AppDirectoryError> {
    let bytes = serde_json::to_vec(cursor)
        .map_err(|_| AppDirectoryError::Corrupt("cursor could not be encoded"))?;
    if bytes.len() > MAX_DIRECTORY_CURSOR_DECODED_BYTES {
        return Err(AppDirectoryError::Corrupt("cursor exceeded its bound"));
    }
    let encoded = URL_SAFE_NO_PAD.encode(bytes);
    if encoded.len() > MAX_DIRECTORY_CURSOR_BYTES {
        return Err(AppDirectoryError::Corrupt("cursor exceeded its bound"));
    }
    Ok(encoded)
}

fn decode_cursor(value: &str) -> Result<DirectoryCursor, AppDirectoryError> {
    if value.is_empty() || value.len() > MAX_DIRECTORY_CURSOR_BYTES {
        return Err(AppDirectoryError::InvalidCursor);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AppDirectoryError::InvalidCursor)?;
    if bytes.len() > MAX_DIRECTORY_CURSOR_DECODED_BYTES {
        return Err(AppDirectoryError::InvalidCursor);
    }
    let cursor: DirectoryCursor =
        serde_json::from_slice(&bytes).map_err(|_| AppDirectoryError::InvalidCursor)?;
    if cursor.version != 1
        || cursor.sort_at.len() > 64
        || cursor.sort_at.chars().any(char::is_control)
    {
        return Err(AppDirectoryError::InvalidCursor);
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::super::{
        lifecycle::AppInstallationLifecycle,
        manifest::{AppManifestNavigationPlacement, AppManifestNavigationSurface, AppRoute},
        models::{AppReference, AppRevision},
        records::{AppCompatibilityRequirement, AppPackageSourceKind},
    };
    use super::*;

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 15, 0, 0, second)
            .single()
            .unwrap()
    }

    fn navigation_declaration() -> AppManifestNavigationEntry {
        AppManifestNavigationEntry {
            id: AppName::parse("meetings_console").unwrap(),
            title: "Meetings".to_owned(),
            route: AppRoute::parse("/meetings-console").unwrap(),
            placement: AppManifestNavigationPlacement::Section {
                section: AppName::parse("observe").unwrap(),
            },
            surface: AppManifestNavigationSurface::CustomSurface {
                entry_point: AppRoute::parse("/console").unwrap(),
                fallback_view: AppName::parse("sessions").unwrap(),
            },
        }
    }

    /// A candidate shaped like the seeded system meetings package: one
    /// declared console, in the lifecycle state under test.
    fn navigation_candidate(status: AppInstallationStatus) -> DirectoryCandidate {
        let enabled = status == AppInstallationStatus::Enabled;
        let revision = || AppRevision::new(1).unwrap();
        let package_revision_ref = AppReference::parse("package:meetings@1").unwrap();
        DirectoryCandidate {
            installation: AppInstallation {
                scope: AppScope {
                    principal: AppReference::parse("principal:anonymous").unwrap(),
                    workspace: AppReference::parse("workspace:default").unwrap(),
                },
                installation_id: AppInstallationId::parse("meetings").unwrap(),
                package_revision_ref: package_revision_ref.clone(),
                lifecycle: AppInstallationLifecycle {
                    status,
                    generation: 1,
                    update_return_status: None,
                },
                grant_revision: enabled.then(revision),
                active_schema_revision: enabled.then(revision),
                active_surface_revision: enabled.then(revision),
                created_at: time(1),
                updated_at: time(2),
                disabled_at: None,
                quarantined_at: None,
                uninstalled_at: None,
                purged_at: None,
            },
            package: AppPackageRevision {
                package_id: AppReference::parse("app:meetings").unwrap(),
                semantic_version: "1.0.0".to_owned(),
                content_digest: AppDigest::blake3(b"content"),
                manifest_schema_version: "1".to_owned(),
                authoring_sdk_version: "1".to_owned(),
                publisher_identity: AppReference::parse("publisher:host").unwrap(),
                source_kind: AppPackageSourceKind::Bundled,
                compatibility: vec![AppCompatibilityRequirement {
                    contract: AppName::parse("magician_contract").unwrap(),
                    requirement: "1".to_owned(),
                }],
                requested_authority_digest: AppDigest::blake3(b"authority"),
                requested_data_policy_digest: AppDigest::blake3(b"policy"),
                dependency_lock_digest: AppDigest::blake3(b"lock"),
                entity_schema_digest: AppDigest::blake3(b"entity"),
                view_schema_digest: AppDigest::blake3(b"view"),
                workflow_digest: AppDigest::blake3(b"workflow"),
                verification_attestation_ref: None,
                conformance_attestation_ref: AppReference::parse("attestation:conformance")
                    .unwrap(),
                created_at: time(1),
            },
            metadata: Some(AppPackageDirectoryMetadata {
                package_revision_ref,
                name: AppName::parse("meetings").unwrap(),
                description: "Meetings".to_owned(),
                actions: Vec::new(),
                custom_surface_entry_count: 1,
                navigation: vec![navigation_declaration()],
                manifest_digest: AppDigest::blake3(b"manifest"),
                created_at: time(1),
            }),
            last_opened_at: None,
            attention_reason: None,
            storage: AppDirectoryStorageSummary {
                record_count: 0,
                revision_count: 0,
                payload_bytes: 0,
                attachment_bytes: 0,
            },
            sort_at: "2026-08-15T00:00:00.000000000Z".to_owned(),
        }
    }

    /// Gate S3 lives or dies on this projection. The shell mounts only what a
    /// directory entry carries, so a manifest declaration the directory never
    /// repeats is a console that exists in review material and nowhere else —
    /// which is exactly how the field's absence made the whole client-side
    /// mounting path unreachable. The second half is the fence: an
    /// installation the owner has not granted contributes no first-party
    /// chrome, mirroring `views` above.
    #[test]
    fn directory_projects_declared_navigation_only_for_an_enabled_installation() {
        let entry = build_entry(
            navigation_candidate(AppInstallationStatus::Enabled),
            Vec::new(),
            None,
            None,
            None,
        )
        .expect("enabled candidate builds");
        let wire = serde_json::to_value(&entry).expect("entry serializes");
        let declared = wire["navigation"]
            .as_array()
            .expect("the wire carries the declaration");
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0]["id"], "meetings_console");
        assert_eq!(declared[0]["title"], "Meetings");
        assert_eq!(declared[0]["route"], "/meetings-console");
        assert_eq!(declared[0]["placement"]["kind"], "section");
        assert_eq!(declared[0]["placement"]["section"], "observe");
        assert_eq!(declared[0]["surface"]["kind"], "custom_surface");
        assert_eq!(declared[0]["surface"]["entry_point"], "/console");
        assert_eq!(declared[0]["surface"]["fallback_view"], "sessions");

        for parked in [
            AppInstallationStatus::ReadyForReview,
            AppInstallationStatus::Disabled,
            AppInstallationStatus::Quarantined,
        ] {
            let entry = build_entry(navigation_candidate(parked), Vec::new(), None, None, None)
                .expect("parked candidate builds");
            let wire = serde_json::to_value(&entry).expect("entry serializes");
            assert!(
                wire.get("navigation").is_none(),
                "{parked:?} must contribute no first-party navigation"
            );
        }
    }

    fn query(search: Option<&str>) -> AppDirectoryQuery {
        AppDirectoryQuery {
            section: AppDirectorySection::Installed,
            pinned_target_kind: None,
            search: search.map(str::to_owned),
            limit: DEFAULT_APP_DIRECTORY_LIMIT,
            cursor: None,
        }
    }

    #[test]
    fn query_and_cursor_bounds_fail_closed() {
        let mut invalid = query(Some(&"x".repeat(MAX_DIRECTORY_SEARCH_BYTES + 1)));
        assert!(matches!(
            invalid.validate(),
            Err(AppDirectoryError::InvalidQuery(_))
        ));
        invalid = query(Some("line\nbreak"));
        assert!(invalid.validate().is_err());
        invalid = query(None);
        invalid.limit = MAX_APP_DIRECTORY_LIMIT + 1;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn cursor_is_bounded_and_bound_to_section_and_search() {
        let cursor = DirectoryCursor {
            version: 1,
            section: AppDirectorySection::Pinned,
            pinned_target_kind: Some(AppDirectoryTargetKind::View),
            search_digest: AppDigest::blake3(b"plan"),
            sort_at: "2026-08-17T00:00:00.000000000Z".to_owned(),
            installation_id: AppInstallationId::parse("install-plan").unwrap(),
        };
        let encoded = encode_cursor(&cursor).unwrap();
        assert!(encoded.len() <= MAX_DIRECTORY_CURSOR_BYTES);
        assert_eq!(
            decode_cursor(&encoded).unwrap().section,
            AppDirectorySection::Pinned
        );

        let mut request = AppDirectoryQuery {
            section: AppDirectorySection::Installed,
            pinned_target_kind: None,
            search: Some("plan".to_owned()),
            limit: 12,
            cursor: Some(encoded),
        };
        request.validate().unwrap();
        let decoded = decode_cursor(request.cursor.as_deref().unwrap()).unwrap();
        assert_ne!(decoded.section, request.section);
        request.cursor = Some("not-base64***".to_owned());
        assert!(decode_cursor(request.cursor.as_deref().unwrap()).is_err());
    }

    #[test]
    fn metadata_search_escapes_sql_wildcards_and_labels_are_deterministic() {
        assert_eq!(escape_like_pattern("100%_done\\ok"), "100\\%\\_done\\\\ok");
        assert_eq!(humanize_identifier("expand_node"), "Expand Node");
        assert_eq!(humanize_identifier("quick-plan"), "Quick Plan");
    }

    #[test]
    fn directory_page_byte_budget_shortens_instead_of_overflowing() {
        let nearly_full = MAX_DIRECTORY_PAGE_ENTRY_BYTES - 8;
        assert_eq!(
            next_directory_page_bytes(nearly_full, 7).unwrap(),
            Some(MAX_DIRECTORY_PAGE_ENTRY_BYTES)
        );
        assert_eq!(next_directory_page_bytes(nearly_full, 8).unwrap(), None);
        assert!(next_directory_page_bytes(usize::MAX, 1).is_err());
    }
}
