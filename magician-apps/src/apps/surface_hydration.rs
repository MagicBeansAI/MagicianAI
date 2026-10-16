//! Authenticated, indexed hydration for one active compiled app surface.
//!
//! The resolver reads the complete active-generation index from the existing
//! scoped registry owner, verifies its set digest, then decodes and verifies
//! only the selected persisted member before deriving one canonical
//! [`AppQueryRequest`] from compiler-owned MUIJ metadata. It owns no cache,
//! cursor dialect or SQLite connection and never treats a caller-supplied
//! route, scope or envelope as authority.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    entity_adapter::{AppEntityAdapterError, AppEntityAdapterService},
    lifecycle::AppInstallationStatus,
    manifest::{app_route_templates_overlap, AppRoute},
    models::{
        decode_app_contract, AppContractError, AppContractLimits, AppDigest,
        AppExpectedRecordRevision, AppInstallationId, AppMutationAtomicity, AppMutationCommand,
        AppMutationOperation, AppName, AppOrderDirection, AppProtocolVersion, AppQueryPage,
        AppRecordId, AppReference, AppRevision, ValidateAppContract,
    },
    records::{
        AppInstallation, AppMutationReceipt, AppSchemaRevision, AppSurfaceBinding, AppSurfaceStatus,
    },
    registry::{AppRegistryError, AppRegistryService},
    surface_compiler::{
        compiled_surface_set_digest, verify_persisted_surface_member, AppSurfaceCompilerError,
        AppSurfaceEnvelope, AppSurfaceQueryPlan, AppSurfaceQueryPlanError,
    },
};

const MAX_CONCRETE_ROUTE_BYTES: usize = 256;
const MAX_PERSISTED_REFERENCE_BYTES: usize = 192;
const MAX_PERSISTED_NAME_BYTES: usize = 128;
const MAX_PERSISTED_DIGEST_BYTES: usize = 71;
const MAX_CANONICAL_HOST_ROUTE_BYTES: usize = 512;

/// Canonical Phase-3B read response. `page` remains the Phase-0/2
/// `AppQueryPage`; the transport does not invent a surface-specific record or
/// pagination dialect.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppSurfaceHydration {
    binding: AppSurfaceBinding,
    surface: AppSurfaceEnvelope,
    route_parameters: BTreeMap<String, String>,
    /// Durable sequence observed before the indexed page read. A concurrent
    /// mutation can therefore only cause a harmless replay/refetch; it cannot
    /// be skipped by a client that starts consuming deltas from this value.
    change_sequence: u64,
    page: AppQueryPage,
}

impl AppSurfaceHydration {
    pub fn binding(&self) -> &AppSurfaceBinding {
        &self.binding
    }

    pub fn surface(&self) -> &AppSurfaceEnvelope {
        &self.surface
    }

    pub fn route_parameters(&self) -> &BTreeMap<String, String> {
        &self.route_parameters
    }

    pub fn change_sequence(&self) -> u64 {
        self.change_sequence
    }

    pub fn page(&self) -> &AppQueryPage {
        &self.page
    }
}

/// Narrow Phase-3C interaction contract. Scope, installation, entity, schema,
/// grant, provenance and durable mutation identity are all server-derived.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppSurfaceMutationRequest {
    protocol_version: AppProtocolVersion,
    surface_revision: AppRevision,
    view_id: AppName,
    client_mutation_id: AppReference,
    operation: AppSurfaceMutationOperation,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AppSurfaceMutationOperation {
    Create {
        values: Value,
    },
    Update {
        record_id: AppRecordId,
        expected_record_revision: AppRevision,
        patch: Value,
    },
    Delete {
        record_id: AppRecordId,
        expected_record_revision: AppRevision,
    },
    Restore {
        record_id: AppRecordId,
        expected_record_revision: AppRevision,
    },
}

impl ValidateAppContract for AppSurfaceMutationRequest {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.protocol_version != AppProtocolVersion::V1 {
            return Err(AppContractError::invalid(
                "protocol_version",
                "only app protocol v1 is supported",
            ));
        }
        let value = match &self.operation {
            AppSurfaceMutationOperation::Create { values } => Some(values),
            AppSurfaceMutationOperation::Update { patch, .. } => Some(patch),
            AppSurfaceMutationOperation::Delete { .. }
            | AppSurfaceMutationOperation::Restore { .. } => None,
        };
        if let Some(value) = value {
            super::models::validate_json_value(value, limits)?;
            if !value.is_object()
                || matches!(&self.operation, AppSurfaceMutationOperation::Update { .. })
                    && value.as_object().is_some_and(serde_json::Map::is_empty)
            {
                return Err(AppContractError::invalid(
                    "operation",
                    "create values and non-empty update patches must be JSON objects",
                ));
            }
        }
        Ok(())
    }
}

impl AppSurfaceMutationRequest {
    fn into_command(
        self,
        entity: AppName,
        schema_revision: AppRevision,
    ) -> Result<(AppMutationCommand, AppReference), AppContractError> {
        let client_mutation_id = self.client_mutation_id;
        let (operation, expected_record_revisions) = match self.operation {
            AppSurfaceMutationOperation::Create { values } => {
                let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
                    "client_mutation_id": client_mutation_id,
                    "entity": entity,
                }))
                .map_err(|_| {
                    AppContractError::invalid(
                        "client_mutation_id",
                        "could not derive the temporary record identity",
                    )
                })?;
                let hex = digest.as_str().strip_prefix("blake3:").ok_or_else(|| {
                    AppContractError::invalid("client_mutation_id", "invalid digest")
                })?;
                (
                    AppMutationOperation::Create {
                        entity: entity.clone(),
                        temporary_id: AppName::parse(format!("surface_{}", &hex[..24]))?,
                        record_id: None,
                        payload: values,
                    },
                    Vec::new(),
                )
            },
            AppSurfaceMutationOperation::Update {
                record_id,
                expected_record_revision,
                patch,
            } => (
                AppMutationOperation::Update {
                    entity: entity.clone(),
                    record_id: record_id.clone(),
                    patch,
                },
                vec![AppExpectedRecordRevision {
                    entity: entity.clone(),
                    record_id,
                    revision: expected_record_revision,
                }],
            ),
            AppSurfaceMutationOperation::Delete {
                record_id,
                expected_record_revision,
            } => (
                AppMutationOperation::Delete {
                    entity: entity.clone(),
                    record_id: record_id.clone(),
                },
                vec![AppExpectedRecordRevision {
                    entity: entity.clone(),
                    record_id,
                    revision: expected_record_revision,
                }],
            ),
            AppSurfaceMutationOperation::Restore {
                record_id,
                expected_record_revision,
            } => (
                AppMutationOperation::Restore {
                    entity: entity.clone(),
                    record_id: record_id.clone(),
                },
                vec![AppExpectedRecordRevision {
                    entity,
                    record_id,
                    revision: expected_record_revision,
                }],
            ),
        };
        let command = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: client_mutation_id.clone(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: schema_revision,
            operations: vec![operation],
            expected_record_revisions,
        };
        command.validate_app_contract(&AppContractLimits::default())?;
        Ok((command, client_mutation_id))
    }
}

#[derive(Debug, Clone)]
pub struct AppSurfaceHydrationService {
    registry: AppRegistryService,
    entity_adapter: AppEntityAdapterService,
}

#[derive(Debug, Clone)]
pub struct AppActiveSurfaceBindings {
    pub surface_revision: AppRevision,
    pub bindings: Vec<AppSurfaceBinding>,
}

#[derive(Debug, Clone)]
pub(super) struct AppSurfaceDirectoryView {
    pub view_id: AppName,
    pub canonical_host_route: String,
}

impl AppSurfaceHydrationService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self {
            entity_adapter: AppEntityAdapterService::new(registry.clone()),
            registry,
        }
    }

    pub async fn hydrate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        requested_route: &str,
        cursor: Option<AppReference>,
        sort: Option<(AppName, AppOrderDirection)>,
        now: DateTime<Utc>,
    ) -> Result<AppSurfaceHydration, AppSurfaceHydrationError> {
        let requested_route = normalize_concrete_route(requested_route)?;
        let resolved = self
            .resolve_route(authenticated_scope, installation_id, &requested_route, now)
            .await?;
        let request = resolved
            .query_plan
            .request(
                installation_id.clone(),
                cursor,
                &resolved.route_parameters,
                sort.as_ref().map(|(field, direction)| (field, *direction)),
            )
            .map_err(|error| match error {
                AppSurfaceQueryPlanError::InvalidRouteParameters => {
                    AppSurfaceHydrationError::InvalidRoute
                },
                AppSurfaceQueryPlanError::InvalidReadIntent => {
                    AppSurfaceHydrationError::InvalidReadIntent
                },
                AppSurfaceQueryPlanError::Contract(error) => {
                    AppSurfaceHydrationError::Contract(error)
                },
            })?;
        let page = self
            .entity_adapter
            .owner_query(authenticated_scope, request, now)
            .await?;
        ensure_complete_surface_page(&resolved.query_plan, page.next_cursor.is_some())?;
        if page.envelope.installation_id != *installation_id
            || page.envelope.package_revision_ref != resolved.package_revision_ref
            || page.envelope.schema_revision != resolved.schema_revision
        {
            return Err(AppSurfaceHydrationError::RevisionChanged);
        }
        self.ensure_route_is_still_current(authenticated_scope, &resolved, now)
            .await?;
        Ok(AppSurfaceHydration {
            binding: resolved.binding,
            surface: resolved.surface,
            route_parameters: resolved.route_parameters,
            change_sequence: resolved.change_sequence,
            page,
        })
    }

    /// Load and verify the complete active binding generation for derivative
    /// projections such as the Apps directory and published-surface index.
    /// The method returns no entity data and accepts no caller-selected view.
    pub async fn active_surface_bindings(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppActiveSurfaceBindings, AppSurfaceHydrationError> {
        let installation_id = installation_id.clone();
        self.registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(load_active_surface_bindings_blocking(
                    connection,
                    scope,
                    &installation_id,
                ))
            })
            .await?
            .ok_or(AppSurfaceHydrationError::NotFound)?
    }

    pub async fn mutate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        request: AppSurfaceMutationRequest,
        now: DateTime<Utc>,
    ) -> Result<AppMutationReceipt, AppSurfaceMutationError> {
        request.validate_app_contract(&AppContractLimits::default())?;
        let surface_revision = request.surface_revision;
        let view_id = request.view_id.clone();
        let context = self
            .resolve_view(authenticated_scope, installation_id, &view_id, now)
            .await?;
        if context.surface_revision != surface_revision || context.view_id != view_id {
            return Err(AppSurfaceMutationError::RevisionChanged);
        }
        if !context.query_plan.mutation_allowed() {
            return Err(AppSurfaceMutationError::ReadOnlySurface);
        }
        let (command, client_mutation_id) =
            request.into_command(context.query_plan.entity().clone(), context.schema_revision)?;
        let surface_session_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "session_ref": authenticated_scope.session_ref(),
            "installation_id": installation_id,
            "surface_revision": surface_revision,
            "view_id": context.view_id,
        }))
        .map_err(|_| {
            AppContractError::invalid(
                "client_mutation_id",
                "could not derive the surface session identity",
            )
        })?;
        let surface_session_id = AppReference::parse(format!(
            "app-surface-session:{}",
            surface_session_digest.as_str()
        ))?;
        Ok(self
            .entity_adapter
            .surface_mutate(
                authenticated_scope,
                installation_id,
                surface_revision,
                command,
                surface_session_id,
                client_mutation_id,
                now,
            )
            .await?)
    }

    async fn resolve_view(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        view_id: &AppName,
        now: DateTime<Utc>,
    ) -> Result<ResolvedAppSurfaceView, AppSurfaceHydrationError> {
        let installation_id = installation_id.clone();
        let view_id = view_id.clone();
        self.registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(resolve_view_blocking(
                    connection,
                    scope,
                    &installation_id,
                    &view_id,
                ))
            })
            .await?
            .ok_or(AppSurfaceHydrationError::NotFound)?
    }

    async fn resolve_route(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        requested_route: &str,
        now: DateTime<Utc>,
    ) -> Result<ResolvedAppSurfaceRoute, AppSurfaceHydrationError> {
        let installation_id = installation_id.clone();
        let requested_route = requested_route.to_owned();
        self.registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(resolve_route_blocking(
                    connection,
                    scope,
                    &installation_id,
                    &requested_route,
                ))
            })
            .await?
            .ok_or(AppSurfaceHydrationError::NotFound)?
    }

    async fn ensure_route_is_still_current(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        resolved: &ResolvedAppSurfaceRoute,
        now: DateTime<Utc>,
    ) -> Result<(), AppSurfaceHydrationError> {
        let fingerprint = resolved.fingerprint();
        self.registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(verify_current_fingerprint_blocking(
                    connection,
                    scope,
                    &fingerprint,
                ))
            })
            .await?
            .ok_or(AppSurfaceHydrationError::RevisionChanged)??;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum AppSurfaceMutationError {
    #[error("app surface changed before the mutation could be authorized")]
    RevisionChanged,
    #[error("app surface is read-only")]
    ReadOnlySurface,
    #[error(transparent)]
    Hydration(#[from] AppSurfaceHydrationError),
    #[error(transparent)]
    Entity(#[from] AppEntityAdapterError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
}

#[derive(Debug, Error)]
pub enum AppSurfaceHydrationError {
    #[error("app surface does not exist in this authenticated scope")]
    NotFound,
    #[error("app surface route is invalid")]
    InvalidRoute,
    #[error("app surface read intent is invalid")]
    InvalidReadIntent,
    #[error("app surface generation is corrupt: {0}")]
    CorruptGeneration(&'static str),
    #[error("app surface changed during hydration")]
    RevisionChanged,
    #[error("tree surfaces must fit in one bounded canonical page")]
    TreeRecordLimitExceeded,
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Entity(#[from] AppEntityAdapterError),
    #[error(transparent)]
    Compiler(#[from] AppSurfaceCompilerError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

struct ResolvedAppSurfaceRoute {
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    surface_revision: AppRevision,
    compiled_set_digest: AppDigest,
    member_storage_digest: AppDigest,
    query_plan: AppSurfaceQueryPlan,
    binding: AppSurfaceBinding,
    surface: AppSurfaceEnvelope,
    route_parameters: BTreeMap<String, String>,
    change_sequence: u64,
}

struct ResolvedAppSurfaceView {
    schema_revision: AppRevision,
    surface_revision: AppRevision,
    view_id: AppName,
    query_plan: AppSurfaceQueryPlan,
}

impl ResolvedAppSurfaceRoute {
    fn fingerprint(&self) -> ActiveRouteFingerprint {
        ActiveRouteFingerprint {
            installation_id: self.installation_id.clone(),
            package_revision_ref: self.package_revision_ref.clone(),
            schema_revision: self.schema_revision,
            surface_revision: self.surface_revision,
            compiled_set_digest: self.compiled_set_digest.clone(),
            view_id: self.binding.view_id.clone(),
            compiled_view_digest: self.binding.compiled_view_digest.clone(),
            app_local_route: self.binding.app_local_route.clone(),
            canonical_host_route: self.binding.canonical_host_route.clone(),
            member_storage_digest: self.member_storage_digest.clone(),
        }
    }
}

struct ActiveRouteFingerprint {
    installation_id: AppInstallationId,
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    surface_revision: AppRevision,
    compiled_set_digest: AppDigest,
    view_id: AppName,
    compiled_view_digest: AppDigest,
    app_local_route: String,
    canonical_host_route: String,
    member_storage_digest: AppDigest,
}

struct PersistedSurfaceGeneration {
    package_revision_ref: AppReference,
    schema_revision: AppRevision,
    surface_revision: AppRevision,
    compiled_set_digest: AppDigest,
    member_count: usize,
}

struct SurfaceMemberMetadata {
    view_id: AppName,
    app_local_route: String,
    canonical_host_route: String,
    compiled_view_digest: AppDigest,
}

struct VerifiedSurfaceGeneration {
    generation: PersistedSurfaceGeneration,
    schema: AppSchemaRevision,
    members: Vec<SurfaceMemberMetadata>,
}

fn resolve_route_blocking(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    installation_id: &AppInstallationId,
    requested_route: &str,
) -> Result<ResolvedAppSurfaceRoute, AppSurfaceHydrationError> {
    let verified = load_verified_generation(connection, scope, installation_id)?;
    let generation = &verified.generation;
    let computed_set_digest = compiled_surface_set_digest(
        installation_id,
        &generation.package_revision_ref,
        generation.schema_revision,
        generation.surface_revision,
        verified.members.iter().map(|member| {
            (
                &member.view_id,
                member.app_local_route.as_str(),
                &member.compiled_view_digest,
            )
        }),
    )?;
    if computed_set_digest != generation.compiled_set_digest {
        return Err(AppSurfaceHydrationError::Compiler(
            AppSurfaceCompilerError::CompiledSetDigestMismatch,
        ));
    }

    let mut matched = None;
    for member in &verified.members {
        if let Some(route_parameters) =
            match_route_template(&member.app_local_route, requested_route)
        {
            if matched.is_some() {
                return Err(AppSurfaceHydrationError::CorruptGeneration(
                    "multiple active routes match one concrete request",
                ));
            }
            matched = Some((member, route_parameters));
        }
    }
    let (member, route_parameters) = matched.ok_or(AppSurfaceHydrationError::NotFound)?;
    let (binding, surface, member_storage_digest, query_plan) = load_selected_member(
        connection,
        installation_id,
        generation,
        &verified.schema,
        member,
    )?;
    let change_sequence = current_change_sequence_blocking(connection, installation_id)?;
    Ok(ResolvedAppSurfaceRoute {
        installation_id: installation_id.clone(),
        package_revision_ref: generation.package_revision_ref.clone(),
        schema_revision: generation.schema_revision,
        surface_revision: generation.surface_revision,
        compiled_set_digest: generation.compiled_set_digest.clone(),
        member_storage_digest,
        query_plan,
        binding,
        surface,
        route_parameters,
        change_sequence,
    })
}

pub(super) fn load_active_surface_bindings_blocking(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    installation_id: &AppInstallationId,
) -> Result<AppActiveSurfaceBindings, AppSurfaceHydrationError> {
    let verified = load_verified_generation(connection, scope, installation_id)?;
    let generation = &verified.generation;
    let computed_set_digest = compiled_surface_set_digest(
        installation_id,
        &generation.package_revision_ref,
        generation.schema_revision,
        generation.surface_revision,
        verified.members.iter().map(|member| {
            (
                &member.view_id,
                member.app_local_route.as_str(),
                &member.compiled_view_digest,
            )
        }),
    )?;
    if computed_set_digest != generation.compiled_set_digest {
        return Err(AppSurfaceHydrationError::Compiler(
            AppSurfaceCompilerError::CompiledSetDigestMismatch,
        ));
    }
    // Directory and publication projections need only the validated binding
    // index. Decoding every MUIJ envelope here would make metadata discovery
    // proportional to the potentially much larger rendered documents.
    let bindings =
        load_generation_bindings(connection, installation_id, generation, &verified.members)?;
    Ok(AppActiveSurfaceBindings {
        surface_revision: generation.surface_revision,
        bindings,
    })
}

/// Load the directory-visible member index for a bounded installation page in
/// one query. Directory navigation needs only verified identities and routes;
/// decoding every binding document per app would turn a 100-row directory page
/// into hundreds of dependent queries and potentially megabytes of unrelated
/// JSON work.
pub(super) fn load_active_surface_directory_views_blocking(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    installations: &[AppInstallation],
) -> Result<HashMap<AppInstallationId, Vec<AppSurfaceDirectoryView>>, AppSurfaceHydrationError> {
    if installations.is_empty() {
        return Ok(HashMap::new());
    }
    if installations.len() > AppContractLimits::default().max_collection_items() {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "directory installation page exceeds the bounded generation limit",
        ));
    }

    let mut clauses = Vec::with_capacity(installations.len());
    let mut parameters =
        Vec::with_capacity(installations.len().saturating_mul(2).saturating_add(5));
    let mut expected = HashMap::with_capacity(installations.len());
    for installation in installations {
        if installation.scope != *scope
            || installation.lifecycle.status != AppInstallationStatus::Enabled
            || expected
                .insert(installation.installation_id.clone(), installation)
                .is_some()
        {
            return Err(AppSurfaceHydrationError::CorruptGeneration(
                "directory installation identity is invalid or duplicated",
            ));
        }
        let surface_revision = installation.active_surface_revision.ok_or(
            AppSurfaceHydrationError::CorruptGeneration(
                "enabled installation has no active surface revision",
            ),
        )?;
        clauses.push("(g.installation_id = ? AND g.revision = ?)".to_owned());
        parameters.push(SqlValue::Text(installation.installation_id.to_string()));
        parameters.push(SqlValue::Integer(revision_i64(surface_revision)?));
    }
    parameters.push(SqlValue::Integer(limit_i64(MAX_PERSISTED_REFERENCE_BYTES)?));
    parameters.push(SqlValue::Integer(limit_i64(MAX_PERSISTED_DIGEST_BYTES)?));
    parameters.push(SqlValue::Integer(limit_i64(MAX_PERSISTED_NAME_BYTES)?));
    parameters.push(SqlValue::Integer(limit_i64(MAX_CONCRETE_ROUTE_BYTES)?));
    parameters.push(SqlValue::Integer(limit_i64(
        MAX_CANONICAL_HOST_ROUTE_BYTES,
    )?));
    let reference_limit = parameters.len() - 4;
    let digest_limit = parameters.len() - 3;
    let name_limit = parameters.len() - 2;
    let local_route_limit = parameters.len() - 1;
    let host_route_limit = parameters.len();
    let sql = format!(
        "SELECT g.installation_id, g.revision, g.package_revision_ref,
                g.schema_revision, g.compiled_set_digest, g.member_count,
                gm.view_id, gm.app_local_route, gm.canonical_host_route,
                gm.compiled_view_digest
           FROM app_surface_generations g
           JOIN app_surface_generation_members gm
             ON gm.installation_id = g.installation_id AND gm.revision = g.revision
          WHERE ({})
            AND length(g.package_revision_ref) BETWEEN 1 AND ?{reference_limit}
            AND length(g.compiled_set_digest) BETWEEN 1 AND ?{digest_limit}
            AND length(gm.view_id) BETWEEN 1 AND ?{name_limit}
            AND length(gm.app_local_route) BETWEEN 1 AND ?{local_route_limit}
            AND length(gm.canonical_host_route) BETWEEN 1 AND ?{host_route_limit}
            AND length(gm.compiled_view_digest) BETWEEN 1 AND ?{digest_limit}
          ORDER BY g.installation_id ASC, gm.view_id ASC",
        clauses.join(" OR "),
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(parameters), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
        ))
    })?;

    let mut grouped: HashMap<
        AppInstallationId,
        (PersistedSurfaceGeneration, Vec<SurfaceMemberMetadata>),
    > = HashMap::with_capacity(installations.len());
    for row in rows {
        let (
            installation_id,
            surface_revision,
            package_revision_ref,
            schema_revision,
            compiled_set_digest,
            member_count,
            view_id,
            local_route,
            host_route,
            compiled_view_digest,
        ) = row?;
        let installation_id = AppInstallationId::parse(installation_id)?;
        let installation =
            expected
                .get(&installation_id)
                .ok_or(AppSurfaceHydrationError::CorruptGeneration(
                    "directory member belongs to an unexpected installation",
                ))?;
        let surface_revision =
            AppRevision::new(u64::try_from(surface_revision).map_err(|_| {
                AppSurfaceHydrationError::CorruptGeneration("invalid surface revision")
            })?)?;
        let schema_revision = AppRevision::new(u64::try_from(schema_revision).map_err(|_| {
            AppSurfaceHydrationError::CorruptGeneration("invalid schema revision")
        })?)?;
        let member_count = usize::try_from(member_count)
            .map_err(|_| AppSurfaceHydrationError::CorruptGeneration("invalid member count"))?;
        if member_count == 0 || member_count > AppContractLimits::default().max_collection_items() {
            return Err(AppSurfaceHydrationError::CorruptGeneration(
                "member count exceeds the bounded generation limit",
            ));
        }
        let generation = PersistedSurfaceGeneration {
            package_revision_ref: AppReference::parse(package_revision_ref)?,
            schema_revision,
            surface_revision,
            compiled_set_digest: AppDigest::parse(compiled_set_digest)?,
            member_count,
        };
        if installation.active_surface_revision != Some(surface_revision)
            || installation.active_schema_revision != Some(schema_revision)
            || installation.package_revision_ref != generation.package_revision_ref
        {
            return Err(AppSurfaceHydrationError::CorruptGeneration(
                "directory generation does not match the installation tuple",
            ));
        }
        let entry = match grouped.entry(installation_id.clone()) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert((generation, Vec::with_capacity(member_count)))
            },
            std::collections::hash_map::Entry::Occupied(slot) => {
                let entry = slot.into_mut();
                if entry.0.surface_revision != generation.surface_revision
                    || entry.0.schema_revision != generation.schema_revision
                    || entry.0.package_revision_ref != generation.package_revision_ref
                    || entry.0.compiled_set_digest != generation.compiled_set_digest
                    || entry.0.member_count != generation.member_count
                {
                    return Err(AppSurfaceHydrationError::CorruptGeneration(
                        "directory rows mix different active generations",
                    ));
                }
                entry
            },
        };
        push_validated_member_metadata(
            &installation_id,
            &mut entry.1,
            view_id,
            local_route,
            host_route,
            compiled_view_digest,
        )?;
        if entry.1.len() > entry.0.member_count {
            return Err(AppSurfaceHydrationError::CorruptGeneration(
                "generation contains more members than declared",
            ));
        }
    }

    let mut result = HashMap::with_capacity(installations.len());
    for installation in installations {
        let (generation, members) = grouped.remove(&installation.installation_id).ok_or(
            AppSurfaceHydrationError::CorruptGeneration("active generation row is missing"),
        )?;
        if members.len() != generation.member_count {
            return Err(AppSurfaceHydrationError::CorruptGeneration(
                "generation member count differs from the stored set",
            ));
        }
        let computed_set_digest = compiled_surface_set_digest(
            &installation.installation_id,
            &generation.package_revision_ref,
            generation.schema_revision,
            generation.surface_revision,
            members.iter().map(|member| {
                (
                    &member.view_id,
                    member.app_local_route.as_str(),
                    &member.compiled_view_digest,
                )
            }),
        )?;
        if computed_set_digest != generation.compiled_set_digest {
            return Err(AppSurfaceHydrationError::Compiler(
                AppSurfaceCompilerError::CompiledSetDigestMismatch,
            ));
        }
        result.insert(
            installation.installation_id.clone(),
            members
                .into_iter()
                .map(|member| AppSurfaceDirectoryView {
                    view_id: member.view_id,
                    canonical_host_route: member.canonical_host_route,
                })
                .collect(),
        );
    }
    Ok(result)
}

fn current_change_sequence_blocking(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
) -> Result<u64, AppSurfaceHydrationError> {
    let value: i64 = connection.query_row(
        "SELECT COALESCE(
             (SELECT next_change_seq - 1 FROM app_installation_sequences
               WHERE installation_id = ?1),
             (SELECT MAX(change_seq) FROM app_record_heads
               WHERE installation_id = ?1),
             0
         )",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    u64::try_from(value)
        .map_err(|_| AppSurfaceHydrationError::CorruptGeneration("change sequence is negative"))
}

fn ensure_complete_surface_page(
    query_plan: &AppSurfaceQueryPlan,
    has_next_cursor: bool,
) -> Result<(), AppSurfaceHydrationError> {
    if query_plan.requires_complete_page() && has_next_cursor {
        return Err(AppSurfaceHydrationError::TreeRecordLimitExceeded);
    }
    Ok(())
}

fn resolve_view_blocking(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    installation_id: &AppInstallationId,
    view_id: &AppName,
) -> Result<ResolvedAppSurfaceView, AppSurfaceHydrationError> {
    let verified = load_verified_generation(connection, scope, installation_id)?;
    let generation = &verified.generation;
    let computed_set_digest = compiled_surface_set_digest(
        installation_id,
        &generation.package_revision_ref,
        generation.schema_revision,
        generation.surface_revision,
        verified.members.iter().map(|member| {
            (
                &member.view_id,
                member.app_local_route.as_str(),
                &member.compiled_view_digest,
            )
        }),
    )?;
    if computed_set_digest != generation.compiled_set_digest {
        return Err(AppSurfaceHydrationError::Compiler(
            AppSurfaceCompilerError::CompiledSetDigestMismatch,
        ));
    }
    let member = verified
        .members
        .iter()
        .find(|member| &member.view_id == view_id)
        .ok_or(AppSurfaceHydrationError::NotFound)?;
    let (binding, surface, _, query_plan) = load_selected_member(
        connection,
        installation_id,
        generation,
        &verified.schema,
        member,
    )?;
    if binding.view_id != *view_id || surface.view_id() != view_id {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "surface member view identity differs",
        ));
    }
    Ok(ResolvedAppSurfaceView {
        schema_revision: generation.schema_revision,
        surface_revision: generation.surface_revision,
        view_id: view_id.clone(),
        query_plan,
    })
}

fn load_verified_generation(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    installation_id: &AppInstallationId,
) -> Result<VerifiedSurfaceGeneration, AppSurfaceHydrationError> {
    let installation = load_enabled_installation(connection, scope, installation_id)?;
    let surface_revision =
        installation
            .active_surface_revision
            .ok_or(AppSurfaceHydrationError::CorruptGeneration(
                "enabled installation has no active surface revision",
            ))?;
    let schema_revision =
        installation
            .active_schema_revision
            .ok_or(AppSurfaceHydrationError::CorruptGeneration(
                "enabled installation has no active schema revision",
            ))?;
    let generation = load_generation(connection, installation_id, surface_revision)?;
    if generation.package_revision_ref != installation.package_revision_ref
        || generation.schema_revision != schema_revision
    {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "active generation does not match the installation tuple",
        ));
    }
    let schema = load_schema_revision(connection, installation_id, &generation)?;
    let members = load_generation_metadata(connection, installation_id, &generation)?;
    Ok(VerifiedSurfaceGeneration {
        generation,
        schema,
        members,
    })
}

fn load_enabled_installation(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    installation_id: &AppInstallationId,
) -> Result<AppInstallation, AppSurfaceHydrationError> {
    let bytes = connection
        .query_row(
            "SELECT record_json FROM app_installations
              WHERE installation_id = ?1
                AND length(record_json) BETWEEN 1 AND ?2",
            params![
                installation_id.as_str(),
                limit_i64(AppContractLimits::default().max_document_bytes())?,
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(AppSurfaceHydrationError::NotFound)?;
    let installation: AppInstallation = decode_app_contract(&bytes, &AppContractLimits::default())?;
    if installation.scope != *scope || installation.installation_id != *installation_id {
        return Err(AppSurfaceHydrationError::NotFound);
    }
    if installation.lifecycle.status != AppInstallationStatus::Enabled {
        return Err(AppSurfaceHydrationError::NotFound);
    }
    Ok(installation)
}

fn load_generation(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
    surface_revision: AppRevision,
) -> Result<PersistedSurfaceGeneration, AppSurfaceHydrationError> {
    let revision = revision_i64(surface_revision)?;
    let row = connection
        .query_row(
            "SELECT package_revision_ref, schema_revision, compiled_set_digest, member_count
               FROM app_surface_generations
              WHERE installation_id = ?1 AND revision = ?2
                AND length(package_revision_ref) BETWEEN 1 AND ?3
                AND length(compiled_set_digest) BETWEEN 1 AND ?4",
            params![
                installation_id.as_str(),
                revision,
                limit_i64(MAX_PERSISTED_REFERENCE_BYTES)?,
                limit_i64(MAX_PERSISTED_DIGEST_BYTES)?,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppSurfaceHydrationError::CorruptGeneration(
            "active generation row is missing",
        ))?;
    let schema_revision =
        AppRevision::new(u64::try_from(row.1).map_err(|_| {
            AppSurfaceHydrationError::CorruptGeneration("invalid schema revision")
        })?)?;
    let member_count = usize::try_from(row.3)
        .map_err(|_| AppSurfaceHydrationError::CorruptGeneration("invalid member count"))?;
    if member_count == 0 || member_count > AppContractLimits::default().max_collection_items() {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "member count exceeds the bounded generation limit",
        ));
    }
    Ok(PersistedSurfaceGeneration {
        package_revision_ref: AppReference::parse(row.0)?,
        schema_revision,
        surface_revision,
        compiled_set_digest: AppDigest::parse(row.2)?,
        member_count,
    })
}

fn load_schema_revision(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
    generation: &PersistedSurfaceGeneration,
) -> Result<AppSchemaRevision, AppSurfaceHydrationError> {
    let bytes = connection
        .query_row(
            "SELECT record_json FROM app_schema_revisions
              WHERE installation_id = ?1 AND revision = ?2 AND package_revision_ref = ?3
                AND length(record_json) BETWEEN 1 AND ?4",
            params![
                installation_id.as_str(),
                revision_i64(generation.schema_revision)?,
                generation.package_revision_ref.as_str(),
                limit_i64(AppContractLimits::default().max_document_bytes())?,
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .ok_or(AppSurfaceHydrationError::CorruptGeneration(
            "active generation schema row is missing",
        ))?;
    let schema: AppSchemaRevision = decode_app_contract(&bytes, &AppContractLimits::default())?;
    if schema.installation_id != *installation_id
        || schema.revision != generation.schema_revision
        || schema.package_revision_ref != generation.package_revision_ref
    {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "active generation schema identity differs",
        ));
    }
    Ok(schema)
}

fn load_generation_metadata(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
    generation: &PersistedSurfaceGeneration,
) -> Result<Vec<SurfaceMemberMetadata>, AppSurfaceHydrationError> {
    let mut statement = connection.prepare(
        "SELECT view_id, app_local_route, canonical_host_route, compiled_view_digest
           FROM app_surface_generation_members
          WHERE installation_id = ?1 AND revision = ?2
            AND length(view_id) BETWEEN 1 AND ?4
            AND length(app_local_route) BETWEEN 1 AND ?5
            AND length(canonical_host_route) BETWEEN 1 AND ?6
            AND length(compiled_view_digest) BETWEEN 1 AND ?7
          ORDER BY view_id ASC
          LIMIT ?3",
    )?;
    let limit = i64::try_from(generation.member_count.saturating_add(1)).map_err(|_| {
        AppSurfaceHydrationError::CorruptGeneration("member count cannot be bounded")
    })?;
    let rows = statement.query_map(
        params![
            installation_id.as_str(),
            revision_i64(generation.surface_revision)?,
            limit,
            limit_i64(MAX_PERSISTED_NAME_BYTES)?,
            limit_i64(MAX_CONCRETE_ROUTE_BYTES)?,
            limit_i64(MAX_CANONICAL_HOST_ROUTE_BYTES)?,
            limit_i64(MAX_PERSISTED_DIGEST_BYTES)?,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    let mut members = Vec::with_capacity(generation.member_count);
    for row in rows {
        let (view_id, local_route, host_route, compiled_digest) = row?;
        if members.len() >= generation.member_count {
            return Err(AppSurfaceHydrationError::CorruptGeneration(
                "generation contains more members than declared",
            ));
        }
        push_validated_member_metadata(
            installation_id,
            &mut members,
            view_id,
            local_route,
            host_route,
            compiled_digest,
        )?;
    }
    if members.len() != generation.member_count {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "generation member count differs from the stored set",
        ));
    }
    Ok(members)
}

fn push_validated_member_metadata(
    installation_id: &AppInstallationId,
    members: &mut Vec<SurfaceMemberMetadata>,
    view_id: String,
    local_route: String,
    host_route: String,
    compiled_digest: String,
) -> Result<(), AppSurfaceHydrationError> {
    let view_id = AppName::parse(view_id)?;
    let normalized_route = AppRoute::parse(&local_route).map_err(|_| {
        AppSurfaceHydrationError::CorruptGeneration(
            "surface member contains an invalid local route",
        )
    })?;
    if normalized_route.as_str() != local_route {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "surface member local route is not canonical",
        ));
    }
    let expected_host = if local_route == "/" {
        format!("/apps/{}", installation_id.as_str())
    } else {
        format!("/apps/{}{}", installation_id.as_str(), local_route)
    };
    if host_route != expected_host {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "surface member host route differs from its installation namespace",
        ));
    }
    if members
        .iter()
        .any(|existing| app_route_templates_overlap(&existing.app_local_route, &local_route))
    {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "surface generation contains overlapping route templates",
        ));
    }
    members.push(SurfaceMemberMetadata {
        view_id,
        app_local_route: local_route,
        canonical_host_route: host_route,
        compiled_view_digest: AppDigest::parse(compiled_digest)?,
    });
    Ok(())
}

fn load_generation_bindings(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
    generation: &PersistedSurfaceGeneration,
    members: &[SurfaceMemberMetadata],
) -> Result<Vec<AppSurfaceBinding>, AppSurfaceHydrationError> {
    let row_limit = i64::try_from(members.len().saturating_add(1)).map_err(|_| {
        AppSurfaceHydrationError::CorruptGeneration("member count cannot be bounded")
    })?;
    let mut statement = connection.prepare(
        "SELECT view_id, app_local_route, canonical_host_route,
                compiled_view_digest, binding_json
           FROM app_surface_generation_members
          WHERE installation_id = ?1 AND revision = ?2
            AND length(binding_json) BETWEEN 1 AND ?4
          ORDER BY view_id ASC
          LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![
            installation_id.as_str(),
            revision_i64(generation.surface_revision)?,
            row_limit,
            limit_i64(AppContractLimits::default().max_document_bytes())?,
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        },
    )?;
    let mut bindings = Vec::with_capacity(members.len());
    for (index, row) in rows.enumerate() {
        if index >= members.len() {
            return Err(AppSurfaceHydrationError::CorruptGeneration(
                "generation contains more bindings than declared",
            ));
        }
        let (view_id, local_route, host_route, compiled_digest, binding_json) = row?;
        let expected = &members[index];
        if view_id != expected.view_id.as_str()
            || local_route != expected.app_local_route
            || host_route != expected.canonical_host_route
            || compiled_digest != expected.compiled_view_digest.as_str()
        {
            return Err(AppSurfaceHydrationError::CorruptGeneration(
                "surface binding index differs from generation metadata",
            ));
        }
        let binding: AppSurfaceBinding =
            decode_app_contract(&binding_json, &AppContractLimits::default())?;
        if binding.installation_id != *installation_id
            || binding.surface_revision != generation.surface_revision
            || binding.package_revision_ref != generation.package_revision_ref
            || binding.view_id != expected.view_id
            || binding.app_local_route != expected.app_local_route
            || binding.canonical_host_route != expected.canonical_host_route
            || binding.compiled_view_digest != expected.compiled_view_digest
            || binding.status != AppSurfaceStatus::Active
        {
            return Err(AppSurfaceHydrationError::CorruptGeneration(
                "encoded binding differs from generation metadata",
            ));
        }
        bindings.push(binding);
    }
    if bindings.len() != members.len() {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "generation binding count differs from its member index",
        ));
    }
    Ok(bindings)
}

fn load_selected_member(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
    generation: &PersistedSurfaceGeneration,
    schema: &AppSchemaRevision,
    metadata: &SurfaceMemberMetadata,
) -> Result<
    (
        AppSurfaceBinding,
        AppSurfaceEnvelope,
        AppDigest,
        AppSurfaceQueryPlan,
    ),
    AppSurfaceHydrationError,
> {
    let row = connection
        .query_row(
            "SELECT app_local_route, canonical_host_route, compiled_view_digest,
                    binding_json, envelope_json
               FROM app_surface_generation_members
              WHERE installation_id = ?1 AND revision = ?2 AND view_id = ?3
                AND length(binding_json) BETWEEN 1 AND ?4
                AND length(envelope_json) BETWEEN 1 AND ?5",
            params![
                installation_id.as_str(),
                revision_i64(generation.surface_revision)?,
                metadata.view_id.as_str(),
                limit_i64(AppContractLimits::default().max_document_bytes())?,
                limit_i64(AppContractLimits::default().max_value_bytes())?,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(AppSurfaceHydrationError::RevisionChanged)?;
    if row.0 != metadata.app_local_route
        || row.1 != metadata.canonical_host_route
        || row.2 != metadata.compiled_view_digest.as_str()
    {
        return Err(AppSurfaceHydrationError::RevisionChanged);
    }
    let member_storage_digest = persisted_member_storage_digest(&row.3, &row.4);
    let binding: AppSurfaceBinding = decode_app_contract(&row.3, &AppContractLimits::default())?;
    if binding.view_id != metadata.view_id
        || binding.app_local_route != metadata.app_local_route
        || binding.canonical_host_route != metadata.canonical_host_route
        || binding.compiled_view_digest != metadata.compiled_view_digest
        || binding.installation_id != *installation_id
        || binding.surface_revision != generation.surface_revision
        || binding.package_revision_ref != generation.package_revision_ref
        || binding.status != AppSurfaceStatus::Active
    {
        return Err(AppSurfaceHydrationError::CorruptGeneration(
            "surface member columns and encoded binding differ",
        ));
    }
    let surface = AppSurfaceEnvelope::decode_persisted(&row.4)?;
    let query_plan = verify_persisted_surface_member(
        &binding,
        &surface,
        &generation.package_revision_ref,
        generation.schema_revision,
        schema.created_at,
    )?;
    Ok((binding, surface, member_storage_digest, query_plan))
}

fn verify_current_fingerprint_blocking(
    connection: &rusqlite::Connection,
    scope: &super::records::AppScope,
    expected: &ActiveRouteFingerprint,
) -> Result<(), AppSurfaceHydrationError> {
    let installation = load_enabled_installation(connection, scope, &expected.installation_id)?;
    if installation.package_revision_ref != expected.package_revision_ref
        || installation.active_schema_revision != Some(expected.schema_revision)
        || installation.active_surface_revision != Some(expected.surface_revision)
    {
        return Err(AppSurfaceHydrationError::RevisionChanged);
    }
    let current: Option<(String, String, String, String, Vec<u8>, Vec<u8>)> = connection
        .query_row(
            "SELECT g.compiled_set_digest, m.compiled_view_digest,
                    m.app_local_route, m.canonical_host_route,
                    m.binding_json, m.envelope_json
               FROM app_surface_generations g
               JOIN app_surface_generation_members m
                 ON m.installation_id = g.installation_id AND m.revision = g.revision
              WHERE g.installation_id = ?1 AND g.revision = ?2 AND m.view_id = ?3
                AND length(m.binding_json) BETWEEN 1 AND ?4
                AND length(m.envelope_json) BETWEEN 1 AND ?5",
            params![
                expected.installation_id.as_str(),
                revision_i64(expected.surface_revision)?,
                expected.view_id.as_str(),
                limit_i64(AppContractLimits::default().max_document_bytes())?,
                limit_i64(AppContractLimits::default().max_value_bytes())?,
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((set_digest, view_digest, local_route, host_route, binding_json, envelope_json)) =
        current
    else {
        return Err(AppSurfaceHydrationError::RevisionChanged);
    };
    if set_digest != expected.compiled_set_digest.as_str()
        || view_digest != expected.compiled_view_digest.as_str()
        || local_route != expected.app_local_route
        || host_route != expected.canonical_host_route
        || persisted_member_storage_digest(&binding_json, &envelope_json)
            != expected.member_storage_digest
    {
        return Err(AppSurfaceHydrationError::RevisionChanged);
    }
    Ok(())
}

fn persisted_member_storage_digest(binding_json: &[u8], envelope_json: &[u8]) -> AppDigest {
    // Hash each bounded blob first, then hash the two fixed-width digests. This
    // avoids retaining a concatenated copy while still making the post-query
    // fingerprint sensitive to either persisted payload changing.
    let binding_digest = AppDigest::blake3(binding_json);
    let envelope_digest = AppDigest::blake3(envelope_json);
    let mut material =
        String::with_capacity(binding_digest.as_str().len() + envelope_digest.as_str().len());
    material.push_str(binding_digest.as_str());
    material.push_str(envelope_digest.as_str());
    AppDigest::blake3(material.as_bytes())
}

fn normalize_concrete_route(raw: &str) -> Result<String, AppSurfaceHydrationError> {
    if raw.is_empty()
        || raw.len() > MAX_CONCRETE_ROUTE_BYTES
        || !raw.is_ascii()
        || !raw.starts_with('/')
        || raw.contains('?')
        || raw.contains('#')
        || raw.contains('%')
        || raw.contains('\\')
        || raw.contains("//")
        || raw.chars().any(char::is_control)
    {
        return Err(AppSurfaceHydrationError::InvalidRoute);
    }
    if raw == "/" {
        return Ok(raw.to_owned());
    }
    let mut count = 0usize;
    for segment in raw[1..].split('/') {
        count = count.saturating_add(1);
        if segment.is_empty()
            || matches!(segment, "." | "..")
            || segment.starts_with(':')
            || segment.len() > 128
            || !segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(AppSurfaceHydrationError::InvalidRoute);
        }
    }
    if count > 16 {
        return Err(AppSurfaceHydrationError::InvalidRoute);
    }
    Ok(raw.to_owned())
}

fn match_route_template(template: &str, requested: &str) -> Option<BTreeMap<String, String>> {
    if template == "/" || requested == "/" {
        return (template == requested).then(BTreeMap::new);
    }
    let template_segments = template[1..].split('/').collect::<Vec<_>>();
    let requested_segments = requested[1..].split('/').collect::<Vec<_>>();
    if template_segments.len() != requested_segments.len() {
        return None;
    }
    let mut parameters = BTreeMap::new();
    for (template, value) in template_segments.into_iter().zip(requested_segments) {
        if let Some(name) = template.strip_prefix(':') {
            parameters.insert(name.to_owned(), value.to_owned());
        } else if template != value {
            return None;
        }
    }
    Some(parameters)
}

fn revision_i64(revision: AppRevision) -> Result<i64, AppSurfaceHydrationError> {
    i64::try_from(revision.get())
        .map_err(|_| AppSurfaceHydrationError::CorruptGeneration("revision exceeds SQLite range"))
}

fn limit_i64(limit: usize) -> Result<i64, AppSurfaceHydrationError> {
    i64::try_from(limit)
        .map_err(|_| AppSurfaceHydrationError::CorruptGeneration("byte limit exceeds SQLite range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concrete_routes_are_strict_and_template_parameters_are_named() {
        assert_eq!(normalize_concrete_route("/").unwrap(), "/");
        assert!(normalize_concrete_route("/items/%2e%2e").is_err());
        assert!(normalize_concrete_route("/items/:record").is_err());
        assert_eq!(
            match_route_template("/items/:record_id", "/items/record_1")
                .unwrap()
                .get("record_id")
                .map(String::as_str),
            Some("record_1")
        );
        assert!(match_route_template("/items/:record_id", "/other/record_1").is_none());
    }

    #[test]
    fn selected_member_fingerprint_changes_with_either_persisted_payload() {
        let baseline = persisted_member_storage_digest(b"binding", b"envelope");
        assert_ne!(
            baseline,
            persisted_member_storage_digest(b"binding-2", b"envelope")
        );
        assert_ne!(
            baseline,
            persisted_member_storage_digest(b"binding", b"envelope-2")
        );
    }

    #[test]
    fn surface_mutations_derive_the_entity_and_exact_expected_revision() {
        let request = AppSurfaceMutationRequest {
            protocol_version: AppProtocolVersion::V1,
            surface_revision: AppRevision::new(4).unwrap(),
            view_id: AppName::parse("plans").unwrap(),
            client_mutation_id: AppReference::parse("surface-client:update-one").unwrap(),
            operation: AppSurfaceMutationOperation::Update {
                record_id: AppRecordId::parse("record_1").unwrap(),
                expected_record_revision: AppRevision::new(7).unwrap(),
                patch: serde_json::json!({"title": "Updated"}),
            },
        };
        let (command, client_id) = request
            .into_command(
                AppName::parse("plan").unwrap(),
                AppRevision::new(3).unwrap(),
            )
            .unwrap();
        assert_eq!(client_id.as_str(), "surface-client:update-one");
        assert_eq!(command.expected_schema_revision.get(), 3);
        assert!(matches!(
            &command.operations[0],
            AppMutationOperation::Update { entity, record_id, patch }
                if entity.as_str() == "plan"
                    && record_id.as_str() == "record_1"
                    && patch == &serde_json::json!({"title": "Updated"})
        ));
        assert_eq!(command.expected_record_revisions.len(), 1);
        assert_eq!(command.expected_record_revisions[0].revision.get(), 7);
    }

    #[test]
    fn empty_surface_update_is_rejected_before_authority_resolution() {
        let request = AppSurfaceMutationRequest {
            protocol_version: AppProtocolVersion::V1,
            surface_revision: AppRevision::new(1).unwrap(),
            view_id: AppName::parse("plans").unwrap(),
            client_mutation_id: AppReference::parse("surface-client:empty").unwrap(),
            operation: AppSurfaceMutationOperation::Update {
                record_id: AppRecordId::parse("record_1").unwrap(),
                expected_record_revision: AppRevision::new(1).unwrap(),
                patch: serde_json::json!({}),
            },
        };
        assert!(request
            .validate_app_contract(&AppContractLimits::default())
            .is_err());
    }

    #[test]
    fn tree_hydration_refuses_a_partial_page_instead_of_promoting_orphan_roots() {
        let plan = AppSurfaceQueryPlan::tree_for_complete_page_test();
        assert!(matches!(
            ensure_complete_surface_page(&plan, true),
            Err(AppSurfaceHydrationError::TreeRecordLimitExceeded)
        ));
        assert!(ensure_complete_surface_page(&plan, false).is_ok());
    }
}
