//! Governed Phase-2E adapters over the canonical app entity store.
//!
//! HTTP, future surface bridges and generic personal-agent tools share this
//! service. Transports provide canonical query/mutation contracts only. This
//! owner resolves the live installation tuple, mints the move-only store fence
//! and then calls [`AppEntityStoreService`]; no adapter owns SQLite, cursors or
//! mutation semantics.

use chrono::{DateTime, Utc};
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    boundary::{
        authorize_app_owner_store_mutation, authorize_app_owner_store_query,
        authorize_personal_agent_store_query, AppBoundaryError, AppCurrentStoreEvidence,
        AppPersonalAgentReadAuthority,
    },
    entity_mutation::AppEntityMutationError,
    entity_store::{
        ActiveAppEntitySchema, AppEntityStoreError, AppEntityStoreService, AppGovernedQueryPage,
        AppRevalidatedRecordProjection,
    },
    models::{
        AppComparisonOperator, AppDataEnvelope, AppFieldPath, AppMutationCommand, AppPredicateNode,
        AppQueryPage, AppQueryRequest, AppRecordProjection, AppReference, ValidateAppContract,
    },
    records::{
        AppEntityProjectionGrant, AppMutationOrigin, AppMutationReceipt, AppPersonalAgentAccess,
        AppRunBinding,
    },
    registry::AppRegistryService,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppGenericDataToolOperation {
    Query,
    Search,
}

/// Shared governed data-plane entry point.
#[derive(Debug, Clone)]
pub struct AppEntityAdapterService {
    store: AppEntityStoreService,
}

/// Read-only, server-derived evidence that an exact workflow mutation already
/// committed. It is deliberately not serializable/deserializable; callers use
/// it only to construct the resource authority's committed-recovery proof.
pub struct AppVerifiedWorkflowMutationReceipt {
    receipt: Option<AppMutationReceipt>,
    expected_origin: AppMutationOrigin,
    expected_mutation_key: super::models::AppDigest,
    expected_batch_digest: super::models::AppDigest,
}

impl std::fmt::Debug for AppVerifiedWorkflowMutationReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppVerifiedWorkflowMutationReceipt")
            .field(
                "receipt_id",
                &self.receipt.as_ref().map(|receipt| &receipt.receipt_id),
            )
            .field("mutation_key", &self.expected_mutation_key)
            .field("batch_digest", &self.expected_batch_digest)
            .finish()
    }
}

impl AppVerifiedWorkflowMutationReceipt {
    pub fn receipt(&self) -> Option<&AppMutationReceipt> {
        self.receipt.as_ref()
    }

    pub fn expected_origin(&self) -> &AppMutationOrigin {
        &self.expected_origin
    }

    pub fn expected_mutation_key(&self) -> &super::models::AppDigest {
        &self.expected_mutation_key
    }

    pub fn expected_batch_digest(&self) -> &super::models::AppDigest {
        &self.expected_batch_digest
    }
}

/// Generic tool-facing façade. It deliberately carries no ambient scope or
/// provider identity; every invocation must bring freshly resolved server
/// authority and therefore cannot be reused by an app workflow or delegated
/// worker merely because the tool exists in a catalog.
#[derive(Debug, Clone)]
pub struct AppGenericDataToolAdapter {
    service: AppEntityAdapterService,
}

impl AppGenericDataToolAdapter {
    pub fn new(registry: AppRegistryService) -> Self {
        Self {
            service: AppEntityAdapterService::new(registry),
        }
    }

    pub async fn query(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppQueryPage, AppEntityAdapterError> {
        self.service
            .personal_agent_read(
                authenticated_scope,
                authority,
                AppGenericDataToolOperation::Query,
                request,
                now,
            )
            .await
    }

    pub async fn query_governed(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedQueryPage, AppEntityAdapterError> {
        self.service
            .personal_agent_read_with_policy(
                authenticated_scope,
                authority,
                AppGenericDataToolOperation::Query,
                request,
                now,
            )
            .await
    }

    pub async fn search(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppQueryPage, AppEntityAdapterError> {
        self.service
            .personal_agent_read(
                authenticated_scope,
                authority,
                AppGenericDataToolOperation::Search,
                request,
                now,
            )
            .await
    }

    pub async fn search_governed(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedQueryPage, AppEntityAdapterError> {
        self.service
            .personal_agent_read_with_policy(
                authenticated_scope,
                authority,
                AppGenericDataToolOperation::Search,
                request,
                now,
            )
            .await
    }
}

impl AppEntityAdapterService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self {
            store: AppEntityStoreService::new(registry),
        }
    }

    /// Authenticated owner query used by the HTTP data plane. The request can
    /// assert an installation only; the scope and store identity come from the
    /// verified session and current registry rows.
    pub async fn owner_query(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppQueryPage, AppEntityAdapterError> {
        let active = self.active(authenticated_scope, &request, now).await?;
        let evidence = current_store_evidence(authenticated_scope, &active, now)?;
        let fence = authorize_app_owner_store_query(evidence, &request)?;
        Ok(self
            .store
            .query(authenticated_scope, fence, request, now)
            .await?)
    }

    /// Authenticated owner mutation. The verified session and canonical
    /// idempotency key produce the durable owner-API origin; caller JSON
    /// never supplies provenance or a store fence.
    pub async fn owner_mutate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &super::models::AppInstallationId,
        command: AppMutationCommand,
        now: DateTime<Utc>,
    ) -> Result<AppMutationReceipt, AppEntityAdapterError> {
        let active = self
            .store
            .active_schema(authenticated_scope, installation_id, now)
            .await?
            .ok_or(AppEntityAdapterError::MissingInstallation)?;
        let evidence = current_store_evidence(authenticated_scope, &active, now)?;
        let origin = AppMutationOrigin::OwnerApi {
            session_ref: authenticated_scope.session_ref().clone(),
            request_ref: command.idempotency_key.clone(),
        };
        let fence = authorize_app_owner_store_mutation(evidence, &command, origin)?;
        Ok(self
            .store
            .mutate(authenticated_scope, fence, command, now)
            .await?)
    }

    /// Authenticated app-surface mutation. The caller supplies only a request
    /// already narrowed to one compiler-verified entity. The active surface
    /// revision becomes part of the move-only store fence and is rechecked
    /// inside the mutation transaction, so an update cannot race a surface
    /// generation replacement and commit under stale UI authority.
    pub async fn surface_mutate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &super::models::AppInstallationId,
        surface_revision: super::models::AppRevision,
        command: AppMutationCommand,
        surface_session_id: AppReference,
        client_mutation_id: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppMutationReceipt, AppEntityAdapterError> {
        let active = self
            .store
            .active_schema(authenticated_scope, installation_id, now)
            .await?
            .ok_or(AppEntityAdapterError::MissingInstallation)?;
        if active.active_surface_revision() != Some(surface_revision) {
            return Err(AppEntityAdapterError::StaleSurfaceRevision);
        }
        let evidence = AppCurrentStoreEvidence::from_trusted_surface(
            authenticated_scope,
            active.installation_id().clone(),
            active.installation_generation(),
            active.package_revision_ref().clone(),
            active.grant_revision(),
            active.schema_revision(),
            surface_revision,
            now,
        )?;
        let origin = AppMutationOrigin::Surface {
            surface_session_id,
            client_mutation_id,
        };
        let fence = authorize_app_owner_store_mutation(evidence, &command, origin)?;
        Ok(self
            .store
            .mutate(authenticated_scope, fence, command, now)
            .await?)
    }

    /// Commit the terminal output of one exact workflow run. Scope,
    /// installation, schema and provenance all come from durable server-owned
    /// evidence; model arguments contain none of those authority fields.
    pub async fn workflow_mutate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        output_revision: super::models::AppRevision,
        source_artifact_refs: Vec<AppReference>,
        command: AppMutationCommand,
        now: DateTime<Utc>,
    ) -> Result<AppMutationReceipt, AppEntityAdapterError> {
        let active = self
            .store
            .active_schema(authenticated_scope, &binding.installation_id, now)
            .await?
            .ok_or(AppEntityAdapterError::MissingInstallation)?;
        if authenticated_scope.scope() != &binding.scope
            || active.installation_id() != &binding.installation_id
            || active.package_revision_ref() != &binding.package_revision_ref
            || active.schema_revision() != binding.schema_revision
            || command.expected_schema_revision != binding.schema_revision
        {
            return Err(AppEntityAdapterError::StaleWorkflowBinding);
        }
        let evidence = current_store_evidence(authenticated_scope, &active, now)?;
        let origin = AppMutationOrigin::Workflow {
            execution_id: binding.execution_id.clone(),
            output_revision,
            source_artifact_refs,
        };
        let fence = authorize_app_owner_store_mutation(evidence, &command, origin)?;
        Ok(self
            .store
            .mutate(authenticated_scope, fence, command, now)
            .await?)
    }

    /// Read-only recovery lookup for an exact workflow mutation. It validates
    /// the authenticated scope against the immutable accepted run binding and
    /// re-derives the same server-owned origin and logical mutation key as
    /// [`Self::workflow_mutate`], but deliberately does not require the grant
    /// to remain dispatch-eligible: an already-visible effect must still be
    /// settled honestly after revocation. This never invokes mutation.
    pub async fn verified_workflow_mutation_receipt(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        binding: &AppRunBinding,
        output_revision: super::models::AppRevision,
        source_artifact_refs: Vec<AppReference>,
        command: &AppMutationCommand,
        now: DateTime<Utc>,
    ) -> Result<AppVerifiedWorkflowMutationReceipt, AppEntityAdapterError> {
        command
            .validate_app_contract(&super::models::AppContractLimits::default())
            .map_err(AppEntityAdapterError::Contract)?;
        if authenticated_scope.scope() != &binding.scope
            || command.expected_schema_revision != binding.schema_revision
        {
            return Err(AppEntityAdapterError::StaleWorkflowBinding);
        }
        let origin = AppMutationOrigin::Workflow {
            execution_id: binding.execution_id.clone(),
            output_revision,
            source_artifact_refs,
        };
        let mutation_key = super::boundary::mutation_key(
            &binding.installation_id,
            &origin,
            &command.idempotency_key,
        )?;
        let batch_digest =
            super::models::AppDigest::blake3_canonical_json(&serde_json::to_value(command)?)?;
        let receipt = self
            .store
            .verified_mutation_receipt(
                authenticated_scope,
                binding.installation_id.clone(),
                mutation_key.clone(),
                batch_digest.clone(),
                origin.clone(),
                now,
            )
            .await?;
        Ok(AppVerifiedWorkflowMutationReceipt {
            receipt,
            expected_origin: origin,
            expected_mutation_key: mutation_key,
            expected_batch_digest: batch_digest,
        })
    }

    /// Generic personal-agent query/search adapter. The execution runtime must
    /// supply a non-deserializable direct-owner authority. Grant projection,
    /// search rights and every predicate/order/relation field are checked
    /// before the store reads records; record-level policy is checked again by
    /// the store before a cursor can be published.
    pub async fn personal_agent_read(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        operation: AppGenericDataToolOperation,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppQueryPage, AppEntityAdapterError> {
        Ok(self
            .personal_agent_read_with_policy(
                authenticated_scope,
                authority,
                operation,
                request,
                now,
            )
            .await?
            .into_parts()
            .0)
    }

    pub async fn personal_agent_read_with_policy(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        operation: AppGenericDataToolOperation,
        request: AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedQueryPage, AppEntityAdapterError> {
        let active = self.active(authenticated_scope, &request, now).await?;
        authorize_personal_agent_projection(&active, &request, operation)?;
        let evidence = current_store_evidence(authenticated_scope, &active, now)?;
        let fence = authorize_personal_agent_store_query(
            authenticated_scope,
            evidence,
            authority,
            &request,
            now,
        )?;
        Ok(self
            .store
            .query_with_policy(authenticated_scope, fence, request, now)
            .await?)
    }

    /// Revalidate one row previously returned by the governed personal-agent
    /// query path. The copied page is provenance evidence only; a fresh
    /// move-only personal-agent authority and a current store fence are still
    /// required before the row can feed another app action.
    pub async fn revalidate_personal_agent_projection(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppQueryRequest,
        expected_envelope: &AppDataEnvelope<Vec<AppRecordProjection>>,
        expected_projection: &AppRecordProjection,
        now: DateTime<Utc>,
    ) -> Result<AppRevalidatedRecordProjection, AppEntityAdapterError> {
        let active = self.active(authenticated_scope, &request, now).await?;
        authorize_personal_agent_projection(&active, &request, AppGenericDataToolOperation::Query)?;
        let evidence = current_store_evidence(authenticated_scope, &active, now)?;
        let fence = authorize_personal_agent_store_query(
            authenticated_scope,
            evidence,
            authority,
            &request,
            now,
        )?;
        Ok(self
            .store
            .revalidate_record_projection(
                authenticated_scope,
                fence,
                request,
                expected_envelope,
                expected_projection,
                now,
            )
            .await?)
    }

    async fn active(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        request: &AppQueryRequest,
        now: DateTime<Utc>,
    ) -> Result<ActiveAppEntitySchema, AppEntityAdapterError> {
        self.store
            .active_schema(authenticated_scope, &request.source_installation_id, now)
            .await?
            .ok_or(AppEntityAdapterError::MissingInstallation)
    }
}

fn current_store_evidence(
    authenticated_scope: &AuthenticatedAppScope,
    active: &ActiveAppEntitySchema,
    now: DateTime<Utc>,
) -> Result<AppCurrentStoreEvidence, AppBoundaryError> {
    AppCurrentStoreEvidence::from_trusted_store(
        authenticated_scope,
        active.installation_id().clone(),
        active.installation_generation(),
        active.package_revision_ref().clone(),
        active.grant_revision(),
        active.schema_revision(),
        now,
    )
}

fn authorize_personal_agent_projection(
    active: &ActiveAppEntitySchema,
    request: &AppQueryRequest,
    operation: AppGenericDataToolOperation,
) -> Result<(), AppEntityAdapterError> {
    if active
        .grant()
        .granted_data_handling_policy
        .personal_agent_access
        != AppPersonalAgentAccess::ApprovedProjection
    {
        return Err(AppEntityAdapterError::PersonalAgentProjectionDenied);
    }

    let source_grant = projection_grant(active, &request.entity)?;
    let mut requires_search = operation == AppGenericDataToolOperation::Search;
    for field in &request.select {
        require_projected_field(source_grant, field)?;
    }
    for order in &request.order {
        require_projected_field(source_grant, &order.field)?;
    }
    if let Some(predicate) = &request.predicate {
        for node in &predicate.nodes {
            match node {
                AppPredicateNode::Compare {
                    field, operator, ..
                } => {
                    require_projected_field(source_grant, field)?;
                    requires_search |= matches!(
                        operator,
                        AppComparisonOperator::Contains | AppComparisonOperator::StartsWith
                    );
                },
                AppPredicateNode::In { field, .. } | AppPredicateNode::IsNull { field, .. } => {
                    require_projected_field(source_grant, field)?;
                },
                AppPredicateNode::All { .. }
                | AppPredicateNode::Any { .. }
                | AppPredicateNode::Not { .. } => {},
            }
        }
    }
    if requires_search && !source_grant.search {
        return Err(AppEntityAdapterError::PersonalAgentSearchDenied);
    }

    for expansion in &request.relation_expansions {
        let relation = AppFieldPath::parse(expansion.relation.as_str())?;
        let mut entity = request.entity.clone();
        for _ in 0..usize::from(expansion.max_depth) {
            let grant = projection_grant(active, &entity)?;
            require_projected_field(grant, &relation)?;
            let runtime = active
                .runtime_contract(&entity)
                .ok_or_else(|| AppEntityAdapterError::UnknownEntity(entity.to_string()))?;
            let target = runtime
                .field(&relation)
                .and_then(|field| field.reference_entity())
                .cloned()
                .ok_or_else(|| {
                    AppEntityAdapterError::UnknownRelation(expansion.relation.to_string())
                })?;
            let target_grant = projection_grant(active, &target)?;
            for field in &expansion.select {
                require_projected_field(target_grant, field)?;
            }
            entity = target;
        }
    }
    Ok(())
}

fn projection_grant<'a>(
    active: &'a ActiveAppEntitySchema,
    entity: &super::models::AppName,
) -> Result<&'a AppEntityProjectionGrant, AppEntityAdapterError> {
    active
        .grant()
        .granted_personal_agent_data_access
        .iter()
        .find(|grant| &grant.entity == entity)
        .ok_or(AppEntityAdapterError::PersonalAgentProjectionDenied)
}

fn require_projected_field(
    grant: &AppEntityProjectionGrant,
    field: &AppFieldPath,
) -> Result<(), AppEntityAdapterError> {
    if grant.fields.contains(field) {
        Ok(())
    } else {
        Err(AppEntityAdapterError::PersonalAgentFieldDenied(
            field.to_string(),
        ))
    }
}

#[derive(Debug, Error)]
pub enum AppEntityAdapterError {
    #[error("app entity adapter authority failed: {0}")]
    Boundary(#[from] AppBoundaryError),
    #[error("app entity adapter query failed: {0}")]
    Store(#[from] AppEntityStoreError),
    #[error("app entity adapter mutation failed: {0}")]
    Mutation(#[from] AppEntityMutationError),
    #[error("app entity adapter contract failed: {0}")]
    Contract(#[from] super::models::AppContractError),
    #[error("app entity adapter canonical JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("app installation does not exist")]
    MissingInstallation,
    #[error("app surface revision is no longer active")]
    StaleSurfaceRevision,
    #[error("app workflow binding is no longer active")]
    StaleWorkflowBinding,
    #[error("app grant does not permit personal-agent projections")]
    PersonalAgentProjectionDenied,
    #[error("app grant does not permit personal-agent search")]
    PersonalAgentSearchDenied,
    #[error("app grant does not permit personal-agent field `{0}`")]
    PersonalAgentFieldDenied(String),
    #[error("app personal-agent projection targets unknown entity `{0}`")]
    UnknownEntity(String),
    #[error("app personal-agent projection targets unknown relation `{0}`")]
    UnknownRelation(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::{
        agents::{AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface},
        apps::{
            boundary::{
                AppAgentProcessingClass, AppDirectOwnerExecutionEvidence,
                AppPersonalAgentProviderGrant, AppPersonalAgentReadAuthority,
            },
            entity_store::tests::{compiled_schema, seed_enabled_installation, seed_records},
            lifecycle::AppInstallationStatus,
            models::{
                AppDataClassification, AppDigest, AppInstallationId, AppMutationAtomicity,
                AppMutationOperation, AppName, AppOrderDirection, AppProtocolVersion,
                AppQueryOrder, AppReference, AppRevision,
            },
            registry::{
                tests::{authenticated_scope, canonical_tempdir, time},
                AppRegistryError,
            },
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    fn request(installation_id: AppInstallationId) -> AppQueryRequest {
        AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: installation_id,
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
            limit: 100,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("personal_agent_query").unwrap(),
        }
    }

    fn personal_agent_authority(
        authenticated: &AuthenticatedAppScope,
        execution_ref: &str,
        processing_class: AppAgentProcessingClass,
        maximum_classification: AppDataClassification,
    ) -> AppPersonalAgentReadAuthority {
        let grant = AppPersonalAgentProviderGrant::from_trusted_provider_registry(
            processing_class,
            maximum_classification,
            AppRevision::new(1).unwrap(),
            AppDigest::blake3(b"test-provider-configuration"),
            time(3),
            time(20),
        )
        .unwrap();
        let invocation = AgentInvocationContext {
            principal: "anonymous".to_owned(),
            workspace: "default".to_owned(),
            source_agent_id: None,
            target_agent_id: "primary".to_owned(),
            surface: InvocationSurface::Chat,
            feature_mode: FeatureMode::None,
            source_kind: InvocationSourceKind::ChatInline,
            chat_session_id: Some("session:1".to_owned()),
            chat_turn_id: Some("turn:1".to_owned()),
        };
        let evidence = AppDirectOwnerExecutionEvidence::from_resolved_execution(
            authenticated,
            &invocation,
            AppReference::parse(execution_ref).unwrap(),
            grant,
            time(4),
        )
        .unwrap();
        AppPersonalAgentReadAuthority::from_current_execution(authenticated, evidence, time(4))
            .unwrap()
    }

    #[tokio::test]
    async fn owner_and_personal_agent_adapters_share_byte_equivalent_query_semantics() {
        let temp = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temp.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled).await;
        seed_records(&registry).await;
        let service = AppEntityAdapterService::new(registry.clone());
        let tool = AppGenericDataToolAdapter::new(registry);
        let request = request(AppInstallationId::parse("install_1").unwrap());

        let owner = service
            .owner_query(&authenticated, request.clone(), time(4))
            .await
            .unwrap();
        let authority = personal_agent_authority(
            &authenticated,
            "execution:adapter-test",
            AppAgentProcessingClass::Deterministic,
            AppDataClassification::Secret,
        );
        let agent = tool
            .query(&authenticated, authority, request, time(4))
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&owner).unwrap(),
            serde_json::to_vec(&agent).unwrap()
        );
    }

    #[tokio::test]
    async fn personal_agent_cannot_project_a_field_outside_the_exact_grant() {
        let temp = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temp.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled).await;
        let service = AppEntityAdapterService::new(registry);
        let mut request = request(AppInstallationId::parse("install_1").unwrap());
        request
            .select
            .push(AppFieldPath::parse("secret_notes").unwrap());
        let authority = personal_agent_authority(
            &authenticated,
            "execution:adapter-denied",
            AppAgentProcessingClass::Deterministic,
            AppDataClassification::Secret,
        );
        assert!(matches!(
            service
                .personal_agent_read(
                    &authenticated,
                    authority,
                    AppGenericDataToolOperation::Query,
                    request,
                    time(4),
                )
                .await,
            Err(AppEntityAdapterError::PersonalAgentFieldDenied(_))
        ));
    }

    fn create_command(idempotency_key: &str, temporary_id: &str) -> AppMutationCommand {
        AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse(idempotency_key).unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Create {
                entity: AppName::parse("item").unwrap(),
                temporary_id: AppName::parse(temporary_id).unwrap(),
                record_id: None,
                payload: serde_json::json!({"title": temporary_id, "status": "open"}),
            }],
            expected_record_revisions: Vec::new(),
        }
    }

    #[tokio::test]
    async fn concurrent_owner_writers_serialize_without_losing_receipts() {
        let temp = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temp.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled).await;
        let service = AppEntityAdapterService::new(registry);
        let first_service = service.clone();
        let second_service = service;
        let first_scope = authenticated.clone();
        let second_scope = authenticated;
        let installation = AppInstallationId::parse("install_1").unwrap();
        let first_installation = installation.clone();
        let second_installation = installation;

        // `Overloaded` is admission backpressure, not a lost write: the scope
        // writer guard has a 250ms admission timeout and the blocking pool is
        // saturated when the whole suite runs, so a refusal here commits
        // nothing and the same command is safe to present again. What this test
        // pins is that whichever attempts are *admitted* serialize -- distinct
        // receipts over disjoint change-seq ranges -- not that admission never
        // pushes back.
        async fn admitted(
            service: &AppEntityAdapterService,
            scope: &AuthenticatedAppScope,
            installation: &AppInstallationId,
            command: AppMutationCommand,
        ) -> AppMutationReceipt {
            for _ in 0..64 {
                match service
                    .owner_mutate(scope, installation, command.clone(), time(4))
                    .await
                {
                    Ok(receipt) => return receipt,
                    Err(
                        AppEntityAdapterError::Store(AppEntityStoreError::Registry(
                            AppRegistryError::Overloaded,
                        ))
                        | AppEntityAdapterError::Mutation(AppEntityMutationError::Registry(
                            AppRegistryError::Overloaded,
                        )),
                    ) => tokio::task::yield_now().await,
                    Err(error) => panic!("owner mutation failed: {error}"),
                }
            }
            panic!("owner mutation never cleared writer admission");
        }

        let (first, second) = tokio::join!(
            admitted(
                &first_service,
                &first_scope,
                &first_installation,
                create_command("mutation:concurrent-a", "concurrent_a"),
            ),
            admitted(
                &second_service,
                &second_scope,
                &second_installation,
                create_command("mutation:concurrent-b", "concurrent_b"),
            )
        );
        assert_ne!(first.receipt_id, second.receipt_id);
        assert!(
            first.change_seq_range.last < second.change_seq_range.first
                || second.change_seq_range.last < first.change_seq_range.first
        );
    }

    #[tokio::test]
    async fn retry_after_response_loss_returns_the_exact_committed_receipt() {
        let temp = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temp.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled).await;
        let service = AppEntityAdapterService::new(registry);
        let installation = AppInstallationId::parse("install_1").unwrap();
        let command = create_command("mutation:response-loss", "response_loss");
        let committed = service
            .owner_mutate(&authenticated, &installation, command.clone(), time(4))
            .await
            .unwrap();
        assert!(matches!(
            &committed.origin,
            AppMutationOrigin::OwnerApi {
                session_ref,
                request_ref,
            } if session_ref == authenticated.session_ref()
                && request_ref.as_str() == "mutation:response-loss"
        ));
        let replayed = service
            .owner_mutate(&authenticated, &installation, command, time(5))
            .await
            .unwrap();
        assert_eq!(committed, replayed);
    }

    #[tokio::test]
    async fn remote_agent_policy_denial_publishes_no_query_cursor() {
        let temp = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temp.path()));
        let authenticated = authenticated_scope("anonymous", "default");
        let (digest, schema) = compiled_schema();
        seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled).await;
        seed_records(&registry).await;
        let tool = AppGenericDataToolAdapter::new(registry.clone());
        let authority = personal_agent_authority(
            &authenticated,
            "execution:remote-agent",
            AppAgentProcessingClass::RemoteModel,
            AppDataClassification::Secret,
        );
        let mut query = request(AppInstallationId::parse("install_1").unwrap());
        query.limit = 1;
        assert!(matches!(
            tool.query(&authenticated, authority, query, time(4)).await,
            Err(AppEntityAdapterError::Store(
                AppEntityStoreError::PersonalAgentPolicyDenied
            ))
        ));
        let cursor_count: i64 = registry
            .execute_scoped_read(&authenticated, &time(4), |connection, _| {
                connection
                    .query_row("SELECT COUNT(*) FROM app_query_cursors", [], |row| {
                        row.get(0)
                    })
                    .map_err(super::super::registry::AppRegistryError::from)
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cursor_count, 0);
    }
}
