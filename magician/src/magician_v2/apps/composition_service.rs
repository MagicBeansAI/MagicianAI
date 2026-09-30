//! Server-owned A-to-B app composition and brokered action launch.
//!
//! The pure mapping broker in [`super::composition`] never opens stores or
//! grants. This service owns the live boundary: it revalidates one exact
//! personal-agent query projection, resolves the destination's current
//! installation/package/grant/action contracts, compiles the mapping, and
//! launches the destination through the canonical workflow owner.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    boundary::{
        AppPersonalAgentPublicationFence, AppPersonalAgentReadAuthority, AppStoreReadAudience,
    },
    composition::{
        broker_action_result_to_destination, broker_source_to_destination, AppBrokeredTransfer,
        AppCompositionAuthorityChainReceipt, AppCompositionDestination, AppCompositionError,
    },
    entity_adapter::{AppEntityAdapterError, AppEntityAdapterService},
    entity_mutation::AppEntityMutationError,
    entity_store::{
        query_policy_influence_fields, AppEntityStoreError, AppEntityStoreService,
        AppRevalidatedRecordProjection,
    },
    lifecycle::AppInstallationStatus,
    manifest::{AppManifestError, AppManifestInputSchema},
    models::{
        validate_json_value, AppActionInvocation, AppActionResult, AppContractError,
        AppContractLimits, AppDataClassification, AppDataEnvelope, AppDataSource, AppDigest,
        AppFieldPath, AppHandlingLabels, AppInstallationId, AppModelActionResult,
        AppModelProcessing, AppName, AppProtocolVersion, AppQueryPage, AppQueryRequest,
        AppRecordId, AppRecordProjection, AppReference, AppRevision, AppRunHandle, AppRunStatus,
        AppSourceRef, AppSourceRefKind, ValidateAppContract,
    },
    observability::{
        AppTraceEvent, AppTraceOperation, AppTraceOutcome, AppTraceRetryClass, AppTraceStage,
    },
    package_staging::{AppPackageStager, AppPackageStagingError},
    recipe_ir::workflow_value_mapping_schema,
    registry::{AppRegistryError, AppRegistryService},
    resource_authority::AppResourceAuthorityError,
    schema_compiler::restrict_policy,
    value_mapping::{
        AppValueFieldContract, AppValueMappingError, AppValueMappingOperation,
        AppValueSchemaContract,
    },
    workflows::{
        AppBrokeredRecoverySource, AppBrokeredWorkflowLaunch, AppBrokeredWorkflowLaunchError,
        AppBrokeredWorkflowRecovery, AppWorkflowCompositionSource, AppWorkflowError,
        AppWorkflowLaunch, AppWorkflowService, APP_ACTION_RUN_REF_PREFIX,
    },
};
use crate::magician_v2::{
    artifact_v2::service::{is_canonical_app_workflow_task_id, ArtifactV2Error},
    execution::agent_resources::AgentResources,
};

/// Supported-public request for composing one canonical terminal action result
/// into a second reviewed app action. The source run is carried by the route so
/// it cannot disagree with this body. Mapping operations are declarative data;
/// the server recompiles them against both current schemas before launch.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppActionCompositionRequest {
    pub destination_installation_id: AppInstallationId,
    pub destination_action_id: AppName,
    pub mapping: Vec<AppValueMappingOperation>,
    pub idempotency_key: AppReference,
    /// Additional destinations in the same server-owned authority chain. The
    /// first hop remains in the four stable V1 fields above so existing
    /// callers keep the exact same wire shape. Each continuation is admitted
    /// only after the preceding canonical result has become composable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<AppActionCompositionHop>,
    /// Optional payload-free cursor read over the canonical composition/run
    /// owners. This is part of the existing compose operation, not a ninth
    /// route and not an app-to-app subscription channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<AppActionCompositionSubscriptionRequest>,
}

pub const MAX_APP_ACTION_COMPOSITION_HOPS: usize = 3;
pub const MAX_APP_ACTION_COMPOSITION_SUBSCRIPTION_PAGE: u16 = 8;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppActionCompositionHop {
    pub destination_installation_id: AppInstallationId,
    pub destination_action_id: AppName,
    pub mapping: Vec<AppValueMappingOperation>,
    pub idempotency_key: AppReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppActionCompositionSubscriptionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<AppReference>,
    #[serde(default = "default_composition_subscription_limit")]
    pub limit: u16,
}

const fn default_composition_subscription_limit() -> u16 {
    MAX_APP_ACTION_COMPOSITION_SUBSCRIPTION_PAGE
}

impl ValidateAppContract for AppActionCompositionRequest {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        validate_composition_mapping(&self.mapping, limits, "mapping")?;
        if self.chain.len() >= MAX_APP_ACTION_COMPOSITION_HOPS {
            return Err(AppContractError::InvalidField {
                field: "chain",
                message: format!(
                    "must contain at most {} continuation hops",
                    MAX_APP_ACTION_COMPOSITION_HOPS - 1
                ),
            });
        }
        let mut idempotency_keys = BTreeSet::from([self.idempotency_key.clone()]);
        let mut destination_installations =
            BTreeSet::from([self.destination_installation_id.clone()]);
        for hop in &self.chain {
            validate_composition_mapping(&hop.mapping, limits, "chain.mapping")?;
            if !idempotency_keys.insert(hop.idempotency_key.clone()) {
                return Err(AppContractError::InvalidField {
                    field: "chain.idempotency_key",
                    message: "must be unique for every hop".to_owned(),
                });
            }
            if !destination_installations.insert(hop.destination_installation_id.clone()) {
                return Err(AppContractError::InvalidField {
                    field: "chain.destination_installation_id",
                    message: "must be unique to prohibit cycles and self-escalation".to_owned(),
                });
            }
        }
        if let Some(subscription) = &self.subscription {
            if subscription.limit == 0
                || subscription.limit > MAX_APP_ACTION_COMPOSITION_SUBSCRIPTION_PAGE
            {
                return Err(AppContractError::InvalidField {
                    field: "subscription.limit",
                    message: format!(
                        "must be between 1 and {MAX_APP_ACTION_COMPOSITION_SUBSCRIPTION_PAGE}"
                    ),
                });
            }
        }
        let value = serde_json::to_value(self).map_err(|error| AppContractError::InvalidJson {
            message: error.to_string(),
        })?;
        validate_json_value(&value, limits)?;
        Ok(())
    }
}

fn validate_composition_mapping(
    mapping: &[AppValueMappingOperation],
    limits: &AppContractLimits,
    field: &'static str,
) -> Result<(), AppContractError> {
    if mapping.is_empty() || mapping.len() > limits.max_collection_items() {
        return Err(AppContractError::InvalidField {
            field,
            message: format!(
                "must contain between 1 and {} operations",
                limits.max_collection_items()
            ),
        });
    }
    Ok(())
}

pub struct AppComposeAndInvokeRequest {
    pub source_projection_handle: AppReference,
    pub source_query: AppQueryRequest,
    pub source_page: AppQueryPage,
    pub source_record_id: Option<AppRecordId>,
    pub destination_installation_id: AppInstallationId,
    pub destination_action_id: AppName,
    pub mapping: Vec<AppValueMappingOperation>,
    pub idempotency_key: AppReference,
}

pub struct AppComposeActionResultRequest {
    pub source_run_ref: AppReference,
    pub destination_installation_id: AppInstallationId,
    pub destination_action_id: AppName,
    pub mapping: Vec<AppValueMappingOperation>,
    pub idempotency_key: AppReference,
    pub(crate) authority_chain: Option<AppCompositionChainAdmission>,
}

/// Move-only authority for one exact hop of a caller-declared bounded chain.
/// Durable task receipts retain its payload-free evidence, but this admission
/// itself can only be minted after the workflow owner reopens all source hops.
pub(crate) struct AppCompositionChainAdmission {
    chain_request_digest: AppDigest,
    hop_index: u8,
    hop_count: u8,
}

impl std::fmt::Debug for AppComposeActionResultRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppComposeActionResultRequest")
            .field("source_run_ref", &self.source_run_ref)
            .field(
                "destination_installation_id",
                &self.destination_installation_id,
            )
            .field("destination_action_id", &self.destination_action_id)
            .field("mapping_operation_count", &self.mapping.len())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AppComposeAndInvokeRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppComposeAndInvokeRequest")
            .field(
                "source_installation_id",
                &self.source_page.envelope.installation_id,
            )
            .field("source_row_count", &self.source_page.envelope.value.len())
            .field("has_source_record_id", &self.source_record_id.is_some())
            .field(
                "destination_installation_id",
                &self.destination_installation_id,
            )
            .field("destination_action_id", &self.destination_action_id)
            .field("mapping_operation_count", &self.mapping.len())
            .finish_non_exhaustive()
    }
}

/// Model-facing launch projection. Canonical workflow results keep their full
/// labeled envelope and provenance in the task sidecars; composition returns
/// only stable control identity and the eligible typed value.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppCompositionWorkflowLaunch {
    pub run_handle: AppRunHandle,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<AppModelActionResult<Value>>,
}

impl From<AppWorkflowLaunch> for AppCompositionWorkflowLaunch {
    fn from(launch: AppWorkflowLaunch) -> Self {
        Self {
            run_handle: launch.run_handle,
            result: launch.result.map(|result| result.into_model_projection()),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppActionCompositionErrorCode {
    OutcomeUnavailable,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppCompositionRetryClass {
    Permanent,
    Transient,
    EffectUncertain,
    Cancelled,
}

impl AppCompositionRetryClass {
    const fn retryable(self) -> bool {
        !matches!(self, Self::Permanent | Self::Cancelled)
    }

    const fn effect_uncertain(self) -> bool {
        matches!(self, Self::EffectUncertain)
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AppCompositionOutcome {
    Launched {
        launch: AppCompositionWorkflowLaunch,
        result_withheld_by_policy: bool,
    },
    Unavailable {
        error_code: AppActionCompositionErrorCode,
        retry_class: AppCompositionRetryClass,
        retryable: bool,
        effect_uncertain: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AppActionResultComposition {
    Waiting {
        source_run: super::models::AppRunHandle,
    },
    Launched {
        source_run: super::models::AppRunHandle,
        launch: AppCompositionWorkflowLaunch,
        result_withheld_by_policy: bool,
    },
    SourceTerminal {
        source_run: super::models::AppRunHandle,
        source_status: AppRunStatus,
    },
    Unavailable {
        source_run: super::models::AppRunHandle,
        error_code: AppActionCompositionErrorCode,
        retry_class: AppCompositionRetryClass,
        retryable: bool,
        effect_uncertain: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppActionCompositionChainProgress {
    pub origin_source_run_ref: AppReference,
    pub active_source_run_ref: AppReference,
    pub active_destination_installation_id: AppInstallationId,
    pub active_destination_action_id: AppName,
    pub hop_index: u8,
    pub hop_count: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppActionCompositionUpdateStatus {
    Waiting,
    Launched,
    SourceTerminal,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppActionCompositionUpdate {
    pub sequence: u64,
    pub source_run_ref: AppReference,
    pub destination_installation_id: AppInstallationId,
    pub destination_action_id: AppName,
    pub status: AppActionCompositionUpdateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_run_ref: Option<AppReference>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppActionCompositionSubscriptionPage {
    pub after_sequence: u64,
    pub through_sequence: u64,
    pub current_sequence: u64,
    pub updates: Vec<AppActionCompositionUpdate>,
    pub has_more: bool,
    pub reset_required: bool,
    pub next_cursor: AppReference,
    pub expires_at: DateTime<Utc>,
}

/// Wire response for the existing compose operation. Flattening preserves the
/// stable status-tagged response while adding chain correlation and an
/// optional payload-free subscription page.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AppActionCompositionResponse {
    #[serde(flatten)]
    pub result: AppActionResultComposition,
    pub chain: AppActionCompositionChainProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<AppActionCompositionSubscriptionPage>,
}

pub(crate) struct AppGovernedActionResultComposition {
    result: AppActionResultComposition,
    effective_policy: super::records::AppDataHandlingPolicy,
}

impl AppGovernedActionResultComposition {
    pub(crate) fn into_parts(
        self,
    ) -> (
        AppActionResultComposition,
        super::records::AppDataHandlingPolicy,
    ) {
        (self.result, self.effective_policy)
    }
}

/// Server-only result wrapper. The public launch is safe to serialize; the
/// complete effective policy returns through the compiled-dispatch side
/// channel and never becomes model-writable JSON.
pub(crate) struct AppGovernedCompositionLaunch {
    outcome: AppCompositionOutcome,
    effective_policy: super::records::AppDataHandlingPolicy,
}

impl AppGovernedCompositionLaunch {
    pub(crate) fn into_parts(
        self,
    ) -> (AppCompositionOutcome, super::records::AppDataHandlingPolicy) {
        (self.outcome, self.effective_policy)
    }
}

enum AppRecoveredComposition {
    Launched {
        launch: AppCompositionWorkflowLaunch,
        result_withheld_by_policy: bool,
        effective_policy: super::records::AppDataHandlingPolicy,
    },
    Unavailable {
        effective_policy: super::records::AppDataHandlingPolicy,
        retry_class: AppCompositionRetryClass,
    },
    LegacyFreshProjectionRequired {
        effective_policy: super::records::AppDataHandlingPolicy,
    },
}

pub(crate) enum AppRecordCompositionRecovery {
    Miss,
    Recovered(AppGovernedCompositionLaunch),
    LegacyFreshProjectionRequired(AppGovernedCompositionLaunch),
}

#[derive(Debug, Clone)]
pub struct AppCompositionService {
    workflow: AppWorkflowService,
    registry: AppRegistryService,
    stager: AppPackageStager,
    entity_adapter: AppEntityAdapterService,
    entity_store: AppEntityStoreService,
}

impl AppCompositionService {
    pub fn new(workflow: AppWorkflowService) -> Self {
        let registry = workflow.registry_service();
        let stager = workflow.package_stager();
        let entity_adapter = workflow.entity_adapter_service();
        let entity_store = workflow.entity_store_service();
        Self {
            stager,
            entity_adapter,
            entity_store,
            workflow,
            registry,
        }
    }

    /// Recover a previously sealed record-composition launch before an
    /// ephemeral projection handle is reopened. A miss is not an error: the
    /// caller must continue through normal handle and live-row revalidation.
    pub(crate) async fn recover_record_composition(
        &self,
        authenticated: &AuthenticatedAppScope,
        authority: &AppPersonalAgentReadAuthority,
        destination_installation_id: &AppInstallationId,
        destination_action_id: &AppName,
        idempotency_key: &AppReference,
        source_projection_handle: &AppReference,
        source_record_id: Option<&AppRecordId>,
        mapping: &[AppValueMappingOperation],
        resources: &AgentResources,
        publication_fence: &AppPersonalAgentPublicationFence,
        calling_profile_name: &str,
        now: DateTime<Utc>,
    ) -> Result<AppRecordCompositionRecovery, AppComposeInvokeError> {
        authority.ensure_current(authenticated, &now)?;
        let caller_audience = AppStoreReadAudience::PersonalAgent {
            execution_ref: authority.execution_ref().clone(),
            processing_class: authority.processing_class(),
            maximum_classification: authority.maximum_classification(),
        };
        let mapping = canonical_mapping(mapping.to_vec())?;
        let mapping_request_digest = mapping_request_digest(&mapping)?;
        let source_request_digest =
            record_source_request_digest(source_projection_handle, source_record_id)?;
        let recovered = self
            .recover_existing_brokered(
                authenticated,
                &caller_audience,
                destination_installation_id,
                destination_action_id,
                idempotency_key,
                &mapping_request_digest,
                &source_request_digest,
                AppBrokeredRecoverySource::EntityRecord,
                resources,
                Some((publication_fence, calling_profile_name)),
                now,
            )
            .await?;
        let Some(recovered) = recovered else {
            return Ok(AppRecordCompositionRecovery::Miss);
        };
        Ok(match recovered {
            AppRecoveredComposition::Launched {
                launch,
                result_withheld_by_policy,
                effective_policy,
            } => AppRecordCompositionRecovery::Recovered(AppGovernedCompositionLaunch {
                outcome: AppCompositionOutcome::Launched {
                    launch,
                    result_withheld_by_policy,
                },
                effective_policy,
            }),
            AppRecoveredComposition::Unavailable {
                effective_policy,
                retry_class,
            } => {
                if retry_class == AppCompositionRetryClass::Cancelled {
                    AppRecordCompositionRecovery::Recovered(cancelled_record_composition(
                        effective_policy,
                    ))
                } else {
                    AppRecordCompositionRecovery::Recovered(unavailable_record_composition(
                        effective_policy,
                        retry_class,
                    ))
                }
            },
            AppRecoveredComposition::LegacyFreshProjectionRequired { effective_policy } => {
                AppRecordCompositionRecovery::LegacyFreshProjectionRequired(
                    unavailable_record_composition(
                        effective_policy,
                        AppCompositionRetryClass::Permanent,
                    ),
                )
            },
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn recover_existing_brokered(
        &self,
        authenticated: &AuthenticatedAppScope,
        caller_audience: &AppStoreReadAudience,
        destination_installation_id: &AppInstallationId,
        destination_action_id: &AppName,
        idempotency_key: &AppReference,
        mapping_request_digest: &AppDigest,
        source_request_digest: &AppDigest,
        expected_source: AppBrokeredRecoverySource,
        resources: &AgentResources,
        personal_agent_publication: Option<(&AppPersonalAgentPublicationFence, &str)>,
        now: DateTime<Utc>,
    ) -> Result<Option<AppRecoveredComposition>, AppComposeInvokeError> {
        authenticated
            .ensure_live_at(&now)
            .map_err(super::boundary::AppBoundaryError::from)?;
        let recovered = self
            .workflow
            .recover_brokered_launch(
                authenticated,
                destination_installation_id,
                destination_action_id,
                idempotency_key,
                mapping_request_digest,
                source_request_digest,
                expected_source,
                resources,
                now,
            )
            .await?;
        let Some(recovered) = recovered else {
            return Ok(None);
        };
        if let Some((publication_fence, calling_profile_name)) = personal_agent_publication {
            let config = resources.magician_config_snapshot();
            let current_audience = super::processing_boundary::reattest_personal_agent_publication(
                &config,
                authenticated,
                calling_profile_name,
                publication_fence,
                Utc::now(),
            )?;
            if &current_audience != caller_audience {
                return Err(
                    super::boundary::AppBoundaryError::StalePersonalAgentProviderGrant.into(),
                );
            }
        } else if !matches!(caller_audience, AppStoreReadAudience::AuthenticatedOwner) {
            return Err(super::boundary::AppBoundaryError::IndirectPersonalAgentExecution.into());
        }
        let launch = match recovered {
            AppBrokeredWorkflowRecovery::Launched(launch) => launch,
            AppBrokeredWorkflowRecovery::LegacyFreshProjectionRequired { transfer_policy } => {
                return Ok(Some(
                    AppRecoveredComposition::LegacyFreshProjectionRequired {
                        effective_policy: transfer_policy,
                    },
                ));
            },
            AppBrokeredWorkflowRecovery::Ready(recovery) => {
                let effective_policy = recovery.transfer_policy().clone();
                if current_execution_cancelled() {
                    return Ok(Some(AppRecoveredComposition::Unavailable {
                        effective_policy,
                        retry_class: AppCompositionRetryClass::Cancelled,
                    }));
                }
                match self
                    .workflow
                    .resume_brokered_recovery(
                        authenticated,
                        recovery,
                        resources,
                        personal_agent_publication
                            .map(|(fence, profile_name)| (fence, profile_name, caller_audience)),
                        now,
                    )
                    .await
                {
                    Ok(launch) => launch,
                    Err(error) => {
                        if brokered_launch_publication_denied(&error) {
                            return Err(AppWorkflowError::PersonalAgentPublicationDenied.into());
                        }
                        return Ok(Some(AppRecoveredComposition::Unavailable {
                            effective_policy,
                            retry_class: brokered_launch_retry_class(&error),
                        }));
                    },
                }
            },
        };
        let (launch, result_withheld_by_policy, effective_policy) =
            filter_brokered_launch_for_caller(launch, caller_audience);
        Ok(Some(AppRecoveredComposition::Launched {
            launch,
            result_withheld_by_policy,
            effective_policy,
        }))
    }

    pub(crate) async fn compose_and_invoke(
        &self,
        authenticated: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppComposeAndInvokeRequest,
        resources: &AgentResources,
        publication_fence: &AppPersonalAgentPublicationFence,
        calling_profile_name: &str,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedCompositionLaunch, AppComposeInvokeError> {
        let correlation_id = request.source_projection_handle.clone();
        let destination_action_id = request.destination_action_id.clone();
        let mapping_digest = canonical_mapping(request.mapping.clone())
            .and_then(|mapping| mapping_request_digest(&mapping))
            .ok();
        let result = self
            .compose_and_invoke_inner(
                authenticated,
                authority,
                request,
                resources,
                publication_fence,
                calling_profile_name,
                now,
            )
            .await;
        trace_record_composition(
            &correlation_id,
            &destination_action_id,
            mapping_digest.as_ref(),
            &result,
        );
        result
    }

    async fn compose_and_invoke_inner(
        &self,
        authenticated: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppComposeAndInvokeRequest,
        resources: &AgentResources,
        publication_fence: &AppPersonalAgentPublicationFence,
        calling_profile_name: &str,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedCompositionLaunch, AppComposeInvokeError> {
        request
            .source_page
            .validate_app_contract(&AppContractLimits::default())?;
        request
            .source_query
            .validate_app_contract(&AppContractLimits::default())?;
        if !request.source_query.relation_expansions.is_empty() {
            return Err(AppComposeInvokeError::UnsupportedSourceRelations);
        }
        let mapping = canonical_mapping(request.mapping)?;
        let source_request_digest = record_source_request_digest(
            &request.source_projection_handle,
            request.source_record_id.as_ref(),
        )?;
        let source_projection =
            select_source_projection(&request.source_page, request.source_record_id.as_ref())?
                .clone();
        select_source_ref(&request.source_page, &source_projection)?;
        // Mapping contracts come from the reviewed query/schema declaration,
        // not from which optional values happen to be present in this row.
        let selected_fields = request.source_query.select.clone();
        let policy_influence_fields = query_policy_influence_fields(&request.source_query);
        let caller_execution_ref = authority.execution_ref().clone();
        let caller_audience = AppStoreReadAudience::PersonalAgent {
            execution_ref: caller_execution_ref.clone(),
            processing_class: authority.processing_class(),
            maximum_classification: authority.maximum_classification(),
        };
        let revalidated = self
            .entity_adapter
            .revalidate_personal_agent_projection(
                authenticated,
                authority,
                request.source_query,
                &request.source_page.envelope,
                &source_projection,
                now,
            )
            .await?;
        let base_failure_policy = revalidated.handling_policy().clone();
        let source_schema = match source_value_schema(&revalidated, &selected_fields) {
            Ok(schema) => schema,
            Err(error) => {
                return Ok(unavailable_record_composition(
                    base_failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };
        let source = match source_envelope(authenticated, &revalidated, &source_schema, now) {
            Ok(source) => source,
            Err(error) => {
                return Ok(unavailable_record_composition(
                    base_failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };
        let failure_policy = match tighten_composition_failure_policy(
            revalidated.handling_policy(),
            &source.handling_labels,
        ) {
            Ok(policy) => policy,
            Err(error) => {
                return Ok(unavailable_record_composition(
                    base_failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };

        let destination = match self
            .resolve_destination(
                authenticated,
                &request.destination_installation_id,
                &request.destination_action_id,
                &mapping,
                now,
            )
            .await
        {
            Ok(destination) => destination,
            Err(error) => {
                return Ok(unavailable_record_composition(
                    failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };
        let transfer_id = match transfer_id(
            &request.idempotency_key,
            &source,
            revalidated.active().installation_generation(),
            &destination,
            &mapping,
        ) {
            Ok(transfer_id) => transfer_id,
            Err(error) => {
                return Ok(unavailable_record_composition(
                    failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };
        let transfer = match broker_source_to_destination(
            authenticated,
            revalidated.read_audience(),
            &source,
            revalidated.handling_policy(),
            &source_schema,
            revalidated.active().installation_generation(),
            &destination,
            mapping,
            transfer_id,
            now,
        )
        .and_then(|transfer| {
            transfer.seal_source_record(
                revalidated.projection(),
                policy_influence_fields,
                source_request_digest,
            )
        }) {
            Ok(transfer) => transfer,
            Err(error) => {
                let error = AppComposeInvokeError::from(error);
                return Ok(unavailable_record_composition(
                    failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };
        let transfer_policy = transfer.admission.effective_policy().clone();
        let AppBrokeredTransfer {
            envelope,
            admission,
            ..
        } = transfer;
        let invocation = AppActionInvocation {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: request.idempotency_key,
            action_id: destination.action_id.clone(),
            action_revision: destination.action_revision,
            input: envelope,
            requested_result_schema_ref: destination.result_schema_ref.clone(),
            caller_surface_or_execution_ref: caller_execution_ref,
        };
        if current_execution_cancelled() {
            return Ok(cancelled_record_composition(transfer_policy));
        }
        let brokered_launch = match self
            .workflow
            .invoke_brokered(
                authenticated,
                &destination.installation_id,
                invocation,
                admission,
                resources,
                Some((publication_fence, calling_profile_name, &caller_audience)),
                now,
            )
            .await
        {
            Ok(launch) => launch,
            Err(error) => {
                if brokered_launch_publication_denied(&error) {
                    return Err(AppWorkflowError::PersonalAgentPublicationDenied.into());
                }
                let retry_class = brokered_launch_retry_class(&error);
                if retry_class == AppCompositionRetryClass::Cancelled {
                    return Ok(cancelled_record_composition(transfer_policy));
                }
                return Ok(unavailable_record_composition(transfer_policy, retry_class));
            },
        };
        let (launch, result_withheld_by_policy, effective_policy) =
            filter_brokered_launch_for_caller(brokered_launch, &caller_audience);
        Ok(AppGovernedCompositionLaunch {
            outcome: AppCompositionOutcome::Launched {
                launch,
                result_withheld_by_policy,
            },
            effective_policy,
        })
    }

    /// Reopen one canonical terminal app result and use it as typed input for
    /// another app action. The source bytes stay server-side throughout; a
    /// waiting result returns only its canonical run handle. Internal callers
    /// retain the legacy one-hop receipt, while the supported-public owner
    /// revalidates and seals every hop in its bounded authority chain.
    pub(crate) async fn compose_action_result_and_invoke(
        &self,
        authenticated: &AuthenticatedAppScope,
        authority: AppPersonalAgentReadAuthority,
        request: AppComposeActionResultRequest,
        resources: &AgentResources,
        publication_fence: &AppPersonalAgentPublicationFence,
        calling_profile_name: &str,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedActionResultComposition, AppComposeInvokeError> {
        authority.ensure_current(authenticated, &now)?;
        let caller_execution_ref = authority.execution_ref().clone();
        let caller_audience = AppStoreReadAudience::PersonalAgent {
            execution_ref: caller_execution_ref.clone(),
            processing_class: authority.processing_class(),
            maximum_classification: authority.maximum_classification(),
        };
        self.compose_action_result_and_invoke_for_audience(
            authenticated,
            request,
            resources,
            caller_audience,
            caller_execution_ref,
            Some((publication_fence, calling_profile_name)),
            now,
        )
        .await
    }

    /// Owner-facing supported-public composition. The authenticated session is
    /// correlation only; destination authority is still reconstructed from the
    /// current reviewed source and destination bindings. Source and mapped
    /// value bytes never cross the HTTP boundary.
    pub async fn compose_action_result_for_owner(
        &self,
        authenticated: &AuthenticatedAppScope,
        source_run_ref: AppReference,
        request: AppActionCompositionRequest,
        resources: &AgentResources,
        now: DateTime<Utc>,
    ) -> Result<AppActionCompositionResponse, AppComposeInvokeError> {
        authenticated
            .ensure_live_at(&now)
            .map_err(super::boundary::AppBoundaryError::from)?;
        request.validate_app_contract(&AppContractLimits::default())?;
        let subscription_request = request.subscription.clone();
        let origin_source_run_ref = source_run_ref.clone();
        let AppActionCompositionRequest {
            destination_installation_id,
            destination_action_id,
            mapping,
            idempotency_key,
            chain,
            subscription: _,
        } = request;
        let mut hops = Vec::with_capacity(1 + chain.len());
        hops.push(AppActionCompositionHop {
            destination_installation_id,
            destination_action_id,
            mapping,
            idempotency_key,
        });
        hops.extend(chain);
        let chain_request_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "source_run_ref": &source_run_ref,
            "hops": &hops,
        }))?;
        let hop_count = u8::try_from(hops.len()).map_err(|_| AppContractError::InvalidField {
            field: "chain",
            message: "contains too many hops".to_owned(),
        })?;
        let chain_deadline = now
            .checked_add_signed(Duration::seconds(30))
            .ok_or(AppWorkflowError::OperationDeadlineUnavailable)?;
        let mut current_source = source_run_ref;
        for (hop_index, hop) in hops.into_iter().enumerate() {
            let hop_now = Utc::now();
            if hop_now >= chain_deadline {
                return Err(AppWorkflowError::OperationDeadlineUnavailable.into());
            }
            let active_destination_installation_id = hop.destination_installation_id.clone();
            let active_destination_action_id = hop.destination_action_id.clone();
            let governed = self
                .compose_action_result_and_invoke_for_audience(
                    authenticated,
                    AppComposeActionResultRequest {
                        source_run_ref: current_source.clone(),
                        destination_installation_id: hop.destination_installation_id,
                        destination_action_id: hop.destination_action_id,
                        mapping: hop.mapping,
                        idempotency_key: hop.idempotency_key,
                        authority_chain: Some(AppCompositionChainAdmission {
                            chain_request_digest: chain_request_digest.clone(),
                            hop_index: u8::try_from(hop_index).map_err(|_| {
                                AppContractError::InvalidField {
                                    field: "chain",
                                    message: "contains too many hops".to_owned(),
                                }
                            })?,
                            hop_count,
                        }),
                    },
                    resources,
                    AppStoreReadAudience::AuthenticatedOwner,
                    authenticated.session_ref().clone(),
                    None,
                    hop_now,
                )
                .await?;
            let is_last = hop_index + 1 == usize::from(hop_count);
            match governed.result {
                AppActionResultComposition::Launched {
                    source_run,
                    launch,
                    result_withheld_by_policy,
                } if !is_last
                    && !result_withheld_by_policy
                    && launch.result.as_ref().is_some_and(|result| {
                        matches!(result.status, super::models::AppActionStatus::Completed)
                    }) =>
                {
                    current_source = launch.run_handle.run_ref.clone();
                    let _ = source_run;
                },
                result => {
                    return build_action_composition_response(
                        authenticated,
                        &origin_source_run_ref,
                        &chain_request_digest,
                        u8::try_from(hop_index).map_err(|_| AppContractError::InvalidField {
                            field: "chain",
                            message: "contains too many hops".to_owned(),
                        })?,
                        hop_count,
                        &active_destination_installation_id,
                        &active_destination_action_id,
                        result,
                        subscription_request.as_ref(),
                        hop_now,
                    );
                },
            }
        }
        Err(AppWorkflowError::CorruptBinding.into())
    }

    #[allow(clippy::too_many_arguments)]
    async fn compose_action_result_and_invoke_for_audience(
        &self,
        authenticated: &AuthenticatedAppScope,
        request: AppComposeActionResultRequest,
        resources: &AgentResources,
        caller_audience: AppStoreReadAudience,
        caller_execution_ref: AppReference,
        personal_agent_publication: Option<(&AppPersonalAgentPublicationFence, &str)>,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedActionResultComposition, AppComposeInvokeError> {
        let source_run_ref = request.source_run_ref.clone();
        let destination_action_id = request.destination_action_id.clone();
        let mapping_digest = canonical_mapping(request.mapping.clone())
            .and_then(|mapping| mapping_request_digest(&mapping))
            .ok();
        let result = self
            .compose_action_result_and_invoke_for_audience_inner(
                authenticated,
                request,
                resources,
                caller_audience,
                caller_execution_ref,
                personal_agent_publication,
                now,
            )
            .await;
        trace_action_result_composition(
            &source_run_ref,
            &destination_action_id,
            mapping_digest.as_ref(),
            &result,
        );
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn compose_action_result_and_invoke_for_audience_inner(
        &self,
        authenticated: &AuthenticatedAppScope,
        request: AppComposeActionResultRequest,
        resources: &AgentResources,
        caller_audience: AppStoreReadAudience,
        caller_execution_ref: AppReference,
        personal_agent_publication: Option<(&AppPersonalAgentPublicationFence, &str)>,
        now: DateTime<Utc>,
    ) -> Result<AppGovernedActionResultComposition, AppComposeInvokeError> {
        if canonical_action_source_task_id(&request.source_run_ref).is_none() {
            return Err(AppWorkflowError::NotWorkflowTask.into());
        }
        let mapping = canonical_mapping(request.mapping)?;
        let mapping_request_digest = mapping_request_digest(&mapping)?;
        let source_request_digest = match request.authority_chain.as_ref() {
            Some(chain) if chain.hop_count > 1 => action_chain_source_request_digest(
                &request.source_run_ref,
                &chain.chain_request_digest,
                chain.hop_index,
                chain.hop_count,
            )?,
            Some(_) | None => action_source_request_digest(&request.source_run_ref)?,
        };
        let source_control = self
            .workflow
            .resolve_run_control(authenticated, request.source_run_ref.as_str(), now)
            .await?;
        let canonical_source_run = source_control.run_handle().clone();
        if let Some(recovered) = self
            .recover_existing_brokered(
                authenticated,
                &caller_audience,
                &request.destination_installation_id,
                &request.destination_action_id,
                &request.idempotency_key,
                &mapping_request_digest,
                &source_request_digest,
                AppBrokeredRecoverySource::ActionResult(canonical_source_run.run_ref.clone()),
                resources,
                personal_agent_publication,
                now,
            )
            .await?
        {
            return Ok(match recovered {
                AppRecoveredComposition::Launched {
                    launch,
                    result_withheld_by_policy,
                    effective_policy,
                } => AppGovernedActionResultComposition {
                    result: AppActionResultComposition::Launched {
                        source_run: canonical_source_run,
                        launch,
                        result_withheld_by_policy,
                    },
                    effective_policy,
                },
                AppRecoveredComposition::Unavailable {
                    effective_policy,
                    retry_class,
                } if retry_class == AppCompositionRetryClass::Cancelled => {
                    cancelled_action_result_composition(canonical_source_run, effective_policy)
                },
                AppRecoveredComposition::Unavailable {
                    effective_policy,
                    retry_class,
                } => unavailable_action_result_composition(
                    canonical_source_run,
                    effective_policy,
                    retry_class,
                ),
                AppRecoveredComposition::LegacyFreshProjectionRequired { effective_policy } => {
                    unavailable_action_result_composition(
                        canonical_source_run,
                        effective_policy,
                        AppCompositionRetryClass::Permanent,
                    )
                },
            });
        }
        let source = self
            .workflow
            .composition_source_for_run(
                authenticated,
                canonical_source_run.run_ref.as_str(),
                resources,
                now,
            )
            .await?;
        let ready = match source {
            AppWorkflowCompositionSource::Waiting {
                run_handle,
                effective_policy,
            } => {
                if effective_policy.personal_agent_access
                    != super::records::AppPersonalAgentAccess::ApprovedProjection
                {
                    return Ok(unavailable_action_result_composition(
                        run_handle,
                        effective_policy,
                        AppCompositionRetryClass::Permanent,
                    ));
                }
                return Ok(AppGovernedActionResultComposition {
                    result: AppActionResultComposition::Waiting {
                        source_run: run_handle,
                    },
                    effective_policy,
                });
            },
            AppWorkflowCompositionSource::TerminalUnavailable {
                run_handle,
                status,
                effective_policy,
            } => {
                if effective_policy.personal_agent_access
                    != super::records::AppPersonalAgentAccess::ApprovedProjection
                {
                    return Ok(unavailable_action_result_composition(
                        run_handle,
                        effective_policy,
                        AppCompositionRetryClass::Permanent,
                    ));
                }
                return Ok(AppGovernedActionResultComposition {
                    result: AppActionResultComposition::SourceTerminal {
                        source_run: run_handle,
                        source_status: status,
                    },
                    effective_policy,
                });
            },
            AppWorkflowCompositionSource::Ready(ready) => ready,
        };
        let (
            source_run,
            source_installation_generation,
            mut source,
            source_policy,
            result_schema,
            source_fence,
            authority_hops,
        ) = ready.into_parts();
        if authority_hops.len() > MAX_APP_ACTION_COMPOSITION_HOPS
            || authority_hops
                .iter()
                .any(|hop| hop.installation_id == request.destination_installation_id)
        {
            return Ok(unavailable_action_result_composition(
                source_run,
                source_policy,
                AppCompositionRetryClass::Permanent,
            ));
        }
        // Failure visibility must be no broader than either the reviewed
        // source policy or the actual sealed labels inherited from task input
        // and protected tool results. The broker performs the same monotone
        // raise for successful transfers, but mapping can fail before then.
        let failure_policy =
            match tighten_composition_failure_policy(&source_policy, &source.handling_labels) {
                Ok(policy) => policy,
                Err(error) => {
                    return Ok(unavailable_action_result_composition(
                        source_run,
                        source_policy,
                        composition_retry_class(&error),
                    ));
                },
            };
        source.expires_at = match now.checked_add_signed(Duration::minutes(10)) {
            Some(expires_at) => Some(expires_at),
            None => {
                return Ok(unavailable_action_result_composition(
                    source_run,
                    failure_policy,
                    AppCompositionRetryClass::Permanent,
                ));
            },
        };
        if let Err(error) = source.validate_app_contract(&AppContractLimits::default()) {
            let error = AppComposeInvokeError::from(error);
            return Ok(unavailable_action_result_composition(
                source_run,
                failure_policy,
                composition_retry_class(&error),
            ));
        }
        if let Err(error) = result_schema.validate_result_value(&source.value) {
            let error = AppComposeInvokeError::from(error);
            return Ok(unavailable_action_result_composition(
                source_run,
                failure_policy,
                composition_retry_class(&error),
            ));
        }
        let source_schema = match source_value_schema_from_manifest_output(&result_schema) {
            Ok(schema) => schema,
            Err(error) => {
                return Ok(unavailable_action_result_composition(
                    source_run,
                    failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };
        let destination = match self
            .resolve_destination(
                authenticated,
                &request.destination_installation_id,
                &request.destination_action_id,
                &mapping,
                now,
            )
            .await
        {
            Ok(destination) => destination,
            Err(error) => {
                return Ok(unavailable_action_result_composition(
                    source_run,
                    failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };
        let transfer_id = match transfer_id(
            &request.idempotency_key,
            &source,
            source_installation_generation,
            &destination,
            &mapping,
        ) {
            Ok(transfer_id) => transfer_id,
            Err(error) => {
                return Ok(unavailable_action_result_composition(
                    source_run,
                    failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };
        let transfer = match broker_action_result_to_destination(
            authenticated,
            &source,
            &source_policy,
            &source_schema,
            source_installation_generation,
            &destination,
            mapping,
            transfer_id,
            now,
        )
        .and_then(|transfer| {
            transfer.seal_source_action_result(source_fence, source_request_digest)
        })
        .and_then(|transfer| match request.authority_chain {
            Some(chain) => transfer.seal_authority_chain(AppCompositionAuthorityChainReceipt {
                chain_request_digest: chain.chain_request_digest,
                hop_index: chain.hop_index,
                hop_count: chain.hop_count,
                source_hops: authority_hops,
            }),
            None => Ok(transfer),
        }) {
            Ok(transfer) => transfer,
            Err(error) => {
                let error = AppComposeInvokeError::from(error);
                return Ok(unavailable_action_result_composition(
                    source_run,
                    failure_policy,
                    composition_retry_class(&error),
                ));
            },
        };
        let transfer_policy = transfer.admission.effective_policy().clone();
        let AppBrokeredTransfer {
            envelope,
            admission,
            ..
        } = transfer;
        let invocation = AppActionInvocation {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: request.idempotency_key,
            action_id: destination.action_id.clone(),
            action_revision: destination.action_revision,
            input: envelope,
            requested_result_schema_ref: destination.result_schema_ref.clone(),
            caller_surface_or_execution_ref: caller_execution_ref,
        };
        if current_execution_cancelled() {
            return Ok(cancelled_action_result_composition(
                source_run,
                transfer_policy,
            ));
        }
        let brokered_launch = match self
            .workflow
            .invoke_brokered(
                authenticated,
                &destination.installation_id,
                invocation,
                admission,
                resources,
                personal_agent_publication
                    .map(|(fence, profile_name)| (fence, profile_name, &caller_audience)),
                now,
            )
            .await
        {
            Ok(launch) => launch,
            Err(error) => {
                if brokered_launch_publication_denied(&error) {
                    return Err(AppWorkflowError::PersonalAgentPublicationDenied.into());
                }
                let retry_class = brokered_launch_retry_class(&error);
                if retry_class == AppCompositionRetryClass::Cancelled {
                    return Ok(cancelled_action_result_composition(
                        source_run,
                        transfer_policy,
                    ));
                }
                return Ok(unavailable_action_result_composition(
                    source_run,
                    transfer_policy,
                    retry_class,
                ));
            },
        };
        let (launch, result_withheld_by_policy, effective_policy) =
            filter_brokered_launch_for_caller(brokered_launch, &caller_audience);
        Ok(AppGovernedActionResultComposition {
            result: AppActionResultComposition::Launched {
                source_run,
                launch,
                result_withheld_by_policy,
            },
            effective_policy,
        })
    }

    async fn resolve_destination(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        action_id: &AppName,
        mapping: &[AppValueMappingOperation],
        now: DateTime<Utc>,
    ) -> Result<AppCompositionDestination, AppComposeInvokeError> {
        let installation = self
            .registry
            .installation(authenticated, installation_id, now)
            .await?
            .ok_or(AppComposeInvokeError::MissingDestinationInstallation)?;
        if installation.lifecycle.status != AppInstallationStatus::Enabled {
            return Err(AppComposeInvokeError::DestinationUnavailable);
        }
        let active = self
            .entity_store
            .active_schema(authenticated, installation_id, now)
            .await?
            .ok_or(AppComposeInvokeError::MissingDestinationInstallation)?;
        if active.installation_generation() != installation.lifecycle.generation
            || active.package_revision_ref() != &installation.package_revision_ref
            || Some(active.schema_revision()) != installation.active_schema_revision
            || Some(active.grant_revision()) != installation.grant_revision
        {
            return Err(AppComposeInvokeError::DestinationChanged);
        }
        let package = self
            .registry
            .package_revision(authenticated, active.package_revision_ref(), now)
            .await?
            .ok_or(AppComposeInvokeError::MissingDestinationPackage)?;
        let staged = self
            .stager
            .load_staged_package(authenticated, package.content_digest.clone(), now)
            .await?;
        let manifest = staged.candidate().manifest().manifest();
        let action = manifest
            .app
            .actions
            .get(action_id)
            .ok_or(AppComposeInvokeError::MissingDestinationAction)?;
        let workflow = manifest
            .app
            .workflows
            .get(&action.workflow)
            .ok_or(AppComposeInvokeError::MissingDestinationWorkflow)?;
        let input_schema = value_schema_from_manifest_input(&workflow.input)?;
        // The live grant is the app-wide ceiling. Input-field overrides may
        // narrow it further, so join the policies for exactly the fields this
        // mapping will populate. The mapping compiler remains authoritative
        // for unknown or duplicate targets.
        let mut destination_policy = active.grant().granted_data_handling_policy.clone();
        for target in mapping.iter().map(mapping_target) {
            let Ok(target_name) = AppName::parse(target.as_str()) else {
                continue;
            };
            let Some(field) = workflow.input.fields.get(&target_name) else {
                continue;
            };
            destination_policy = restrict_policy(destination_policy, field.policy());
        }
        // V1 app workflow model input is always treated as at least sensitive
        // and at most local-only. Fold that execution clamp into the brokered
        // join now so its durable policy digest describes the bytes that will
        // actually enter the workflow, rather than tightening labels later
        // without changing their identity.
        destination_policy.classification_floor = destination_policy
            .classification_floor
            .max(AppDataClassification::Sensitive);
        destination_policy.model_processing = destination_policy
            .model_processing
            .min(AppModelProcessing::LocalOnly);
        Ok(AppCompositionDestination {
            installation_id: installation_id.clone(),
            package_revision_ref: active.package_revision_ref().clone(),
            schema_revision: active.schema_revision(),
            grant_revision: active.grant_revision(),
            action_id: action_id.clone(),
            action_revision: AppRevision::new(installation.lifecycle.generation)?,
            input_schema_ref: action.input_from.clone(),
            input_schema,
            result_schema_ref: action.result_from.clone(),
            policy: destination_policy,
        })
    }
}

fn result_labels_permit_caller(
    caller_audience: &AppStoreReadAudience,
    classification: AppDataClassification,
    model_processing: AppModelProcessing,
) -> bool {
    caller_audience.permits_policy(classification, model_processing)
}

fn brokered_result_labels_permit_caller(
    result: Option<&AppActionResult<Value>>,
    caller_audience: &AppStoreReadAudience,
) -> bool {
    result.is_none_or(|result| {
        result.output.as_ref().is_none_or(|output| {
            result_labels_permit_caller(
                caller_audience,
                output.handling_labels.classification,
                output.handling_labels.model_processing,
            )
        })
    })
}

fn filter_brokered_launch_for_caller(
    brokered: AppBrokeredWorkflowLaunch,
    caller_audience: &AppStoreReadAudience,
) -> (
    AppCompositionWorkflowLaunch,
    bool,
    super::records::AppDataHandlingPolicy,
) {
    let AppBrokeredWorkflowLaunch {
        mut launch,
        transfer_policy,
        result_was_withheld,
        result_policy,
    } = brokered;
    let has_result = launch.result.is_some() || result_was_withheld;
    let result_policy_permits_personal_agent = !has_result
        || result_policy.as_ref().is_some_and(|policy| {
            policy.personal_agent_access
                == super::records::AppPersonalAgentAccess::ApprovedProjection
        });
    let result_labels_permit_personal_agent =
        brokered_result_labels_permit_caller(launch.result.as_ref(), caller_audience);
    let result_withheld_by_policy = result_was_withheld
        || (has_result
            && (!result_policy_permits_personal_agent || !result_labels_permit_personal_agent));
    let effective_policy = if result_withheld_by_policy {
        launch.result = None;
        transfer_policy
    } else {
        result_policy.unwrap_or(transfer_policy)
    };
    (
        AppCompositionWorkflowLaunch::from(launch),
        result_withheld_by_policy,
        effective_policy,
    )
}

fn tighten_composition_failure_policy(
    source_policy: &super::records::AppDataHandlingPolicy,
    labels: &AppHandlingLabels,
) -> Result<super::records::AppDataHandlingPolicy, AppComposeInvokeError> {
    let mut policy = source_policy.clone();
    policy.classification_floor = policy.classification_floor.max(labels.classification);
    policy.model_processing = policy.model_processing.min(labels.model_processing);
    super::records::validate_policy(&policy, &AppContractLimits::default())?;
    Ok(policy)
}

fn unavailable_record_composition(
    effective_policy: super::records::AppDataHandlingPolicy,
    retry_class: AppCompositionRetryClass,
) -> AppGovernedCompositionLaunch {
    AppGovernedCompositionLaunch {
        outcome: AppCompositionOutcome::Unavailable {
            error_code: AppActionCompositionErrorCode::OutcomeUnavailable,
            retryable: retry_class.retryable(),
            effect_uncertain: retry_class.effect_uncertain(),
            retry_class,
        },
        effective_policy,
    }
}

fn cancelled_record_composition(
    effective_policy: super::records::AppDataHandlingPolicy,
) -> AppGovernedCompositionLaunch {
    AppGovernedCompositionLaunch {
        outcome: AppCompositionOutcome::Unavailable {
            error_code: AppActionCompositionErrorCode::Cancelled,
            retry_class: AppCompositionRetryClass::Cancelled,
            retryable: false,
            effect_uncertain: false,
        },
        effective_policy,
    }
}

fn unavailable_action_result_composition(
    source_run: AppRunHandle,
    effective_policy: super::records::AppDataHandlingPolicy,
    retry_class: AppCompositionRetryClass,
) -> AppGovernedActionResultComposition {
    AppGovernedActionResultComposition {
        result: AppActionResultComposition::Unavailable {
            source_run,
            error_code: AppActionCompositionErrorCode::OutcomeUnavailable,
            retryable: retry_class.retryable(),
            effect_uncertain: retry_class.effect_uncertain(),
            retry_class,
        },
        effective_policy,
    }
}

fn cancelled_action_result_composition(
    source_run: AppRunHandle,
    effective_policy: super::records::AppDataHandlingPolicy,
) -> AppGovernedActionResultComposition {
    AppGovernedActionResultComposition {
        result: AppActionResultComposition::Unavailable {
            source_run,
            error_code: AppActionCompositionErrorCode::Cancelled,
            retry_class: AppCompositionRetryClass::Cancelled,
            retryable: false,
            effect_uncertain: false,
        },
        effective_policy,
    }
}

fn trace_record_composition(
    correlation_id: &AppReference,
    destination_action_id: &AppName,
    mapping_digest: Option<&AppDigest>,
    result: &Result<AppGovernedCompositionLaunch, AppComposeInvokeError>,
) {
    let mut trace =
        AppTraceEvent::new(AppTraceStage::Publish, AppTraceOperation::RecordComposition)
            .correlation_id(correlation_id)
            .action_id(destination_action_id);
    if let Some(mapping_digest) = mapping_digest {
        trace = trace.mapping_digest(mapping_digest);
    }
    trace = match result {
        Ok(governed) => match &governed.outcome {
            AppCompositionOutcome::Launched { launch, .. } => trace
                .outcome(AppTraceOutcome::Allowed)
                .child_id(&launch.run_handle.run_ref)
                .retry_class(AppTraceRetryClass::None),
            AppCompositionOutcome::Unavailable {
                retry_class,
                effect_uncertain,
                error_code,
                ..
            } => {
                let outcome = if *error_code == AppActionCompositionErrorCode::Cancelled {
                    AppTraceOutcome::Cancelled
                } else if *effect_uncertain {
                    AppTraceOutcome::Uncertain
                } else {
                    AppTraceOutcome::Unavailable
                };
                trace
                    .outcome(outcome)
                    .retry_class(trace_retry_class(*retry_class))
            },
        },
        Err(error) => trace
            .outcome(AppTraceOutcome::Failed)
            .retry_class(trace_retry_class(composition_retry_class(error))),
    };
    trace.emit();
}

fn trace_action_result_composition(
    source_run_ref: &AppReference,
    destination_action_id: &AppName,
    mapping_digest: Option<&AppDigest>,
    result: &Result<AppGovernedActionResultComposition, AppComposeInvokeError>,
) {
    let mut trace = AppTraceEvent::new(
        AppTraceStage::Publish,
        AppTraceOperation::ActionResultComposition,
    )
    .correlation_id(source_run_ref)
    .action_id(destination_action_id)
    .run_ref(source_run_ref);
    if let Some(mapping_digest) = mapping_digest {
        trace = trace.mapping_digest(mapping_digest);
    }
    trace = match result {
        Ok(governed) => match &governed.result {
            AppActionResultComposition::Waiting { .. } => trace,
            AppActionResultComposition::Launched { launch, .. } => trace
                .outcome(AppTraceOutcome::Allowed)
                .child_id(&launch.run_handle.run_ref)
                .retry_class(AppTraceRetryClass::None),
            AppActionResultComposition::SourceTerminal { source_status, .. } => {
                let outcome = match source_status {
                    AppRunStatus::Cancelled => AppTraceOutcome::Cancelled,
                    AppRunStatus::Uncertain => AppTraceOutcome::Uncertain,
                    AppRunStatus::Failed => AppTraceOutcome::Failed,
                    _ => AppTraceOutcome::Unavailable,
                };
                trace.outcome(outcome)
            },
            AppActionResultComposition::Unavailable {
                retry_class,
                effect_uncertain,
                error_code,
                ..
            } => {
                let outcome = if *error_code == AppActionCompositionErrorCode::Cancelled {
                    AppTraceOutcome::Cancelled
                } else if *effect_uncertain {
                    AppTraceOutcome::Uncertain
                } else {
                    AppTraceOutcome::Unavailable
                };
                trace
                    .outcome(outcome)
                    .retry_class(trace_retry_class(*retry_class))
            },
        },
        Err(error) => trace
            .outcome(AppTraceOutcome::Failed)
            .retry_class(trace_retry_class(composition_retry_class(error))),
    };
    trace.emit();
}

const fn trace_retry_class(retry_class: AppCompositionRetryClass) -> AppTraceRetryClass {
    match retry_class {
        AppCompositionRetryClass::Permanent => AppTraceRetryClass::UserAction,
        AppCompositionRetryClass::Transient => AppTraceRetryClass::SameInput,
        AppCompositionRetryClass::EffectUncertain => AppTraceRetryClass::ReconcileUncertain,
        AppCompositionRetryClass::Cancelled => AppTraceRetryClass::None,
    }
}

fn current_execution_cancelled() -> bool {
    crate::magician_v2::execution::compiled_dispatch::EXECUTION_CANCEL_TOKEN
        .try_with(|token| {
            token
                .as_ref()
                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
        })
        .unwrap_or(false)
}

fn brokered_launch_retry_class(error: &AppBrokeredWorkflowLaunchError) -> AppCompositionRetryClass {
    match error {
        AppBrokeredWorkflowLaunchError::CancelledBeforeDispatch => {
            AppCompositionRetryClass::Cancelled
        },
        AppBrokeredWorkflowLaunchError::PreDispatch(error) => workflow_retry_class(error),
        AppBrokeredWorkflowLaunchError::EffectUncertain(_) => {
            AppCompositionRetryClass::EffectUncertain
        },
    }
}

fn brokered_launch_publication_denied(error: &AppBrokeredWorkflowLaunchError) -> bool {
    matches!(
        error,
        AppBrokeredWorkflowLaunchError::PreDispatch(
            AppWorkflowError::PersonalAgentPublicationDenied
        ) | AppBrokeredWorkflowLaunchError::EffectUncertain(
            AppWorkflowError::PersonalAgentPublicationDenied
        )
    )
}

fn composition_retry_class(error: &AppComposeInvokeError) -> AppCompositionRetryClass {
    match error {
        AppComposeInvokeError::Registry(error) => registry_retry_class(error),
        AppComposeInvokeError::Staging(error) => staging_retry_class(error),
        AppComposeInvokeError::EntityStore(error) => entity_store_retry_class(error),
        AppComposeInvokeError::Entity(error) => entity_adapter_retry_class(error),
        AppComposeInvokeError::DestinationUnavailable
        | AppComposeInvokeError::DestinationChanged => AppCompositionRetryClass::Transient,
        AppComposeInvokeError::Contract(_)
        | AppComposeInvokeError::Manifest(_)
        | AppComposeInvokeError::WorkflowSchema(_)
        | AppComposeInvokeError::Boundary(_)
        | AppComposeInvokeError::Encoding(_)
        | AppComposeInvokeError::Mapping(_)
        | AppComposeInvokeError::Composition(_)
        | AppComposeInvokeError::MissingSourceRecord
        | AppComposeInvokeError::AmbiguousSourceRecord
        | AppComposeInvokeError::SourceRecordRequired
        | AppComposeInvokeError::MissingSourceProvenance
        | AppComposeInvokeError::AmbiguousSourceProvenance
        | AppComposeInvokeError::MissingSourceContract
        | AppComposeInvokeError::EmptySourceResult
        | AppComposeInvokeError::MissingSourceField(_)
        | AppComposeInvokeError::SourceShapeConflict(_)
        | AppComposeInvokeError::UnsupportedSourceRelations
        | AppComposeInvokeError::MissingDestinationInstallation
        | AppComposeInvokeError::MissingDestinationPackage
        | AppComposeInvokeError::MissingDestinationAction
        | AppComposeInvokeError::MissingDestinationWorkflow
        | AppComposeInvokeError::ClockOverflow => AppCompositionRetryClass::Permanent,
        // This helper is used only before dispatch. Once invoke_brokered is
        // entered, the caller is explicitly returned EffectUncertain instead.
        AppComposeInvokeError::Workflow(error) => workflow_retry_class(error),
    }
}

fn registry_retry_class(error: &AppRegistryError) -> AppCompositionRetryClass {
    match error {
        AppRegistryError::Overloaded
        | AppRegistryError::WorkerTerminated(_)
        | AppRegistryError::ScopeBindingCommitStateUnknown
        | AppRegistryError::CompareAndSwapLost(_)
        | AppRegistryError::GenerationConflict { .. }
        | AppRegistryError::OutboxLeaseStale
        | AppRegistryError::Io(_)
        | AppRegistryError::Sqlite(_) => AppCompositionRetryClass::Transient,
        AppRegistryError::Authentication(_)
        | AppRegistryError::Contract(_)
        | AppRegistryError::Approval(_)
        | AppRegistryError::Lifecycle(_)
        | AppRegistryError::PackageLock(_)
        | AppRegistryError::InvalidPublication(_)
        | AppRegistryError::UnsafePath(_)
        | AppRegistryError::StagedPackageInvalid(_)
        | AppRegistryError::ScopeCollision
        | AppRegistryError::AtRestEncryptionKeyUnavailable
        | AppRegistryError::AtRestEncryptionFailed
        | AppRegistryError::IncompatibleDatabase { .. }
        | AppRegistryError::LegacyResourceTreesRequireExplicitMigration { .. }
        | AppRegistryError::IncompleteResourceBaselinesRequireExplicitMigration { .. }
        | AppRegistryError::IdentityConflict { .. }
        | AppRegistryError::InvalidControlPlane(_)
        | AppRegistryError::StateConflict(_)
        | AppRegistryError::MissingRecord { .. }
        | AppRegistryError::Encoding(_) => AppCompositionRetryClass::Permanent,
    }
}

fn staging_retry_class(error: &AppPackageStagingError) -> AppCompositionRetryClass {
    match error {
        AppPackageStagingError::Overloaded
        | AppPackageStagingError::WorkerTerminated(_)
        | AppPackageStagingError::SourceChanged
        | AppPackageStagingError::DestinationChanged
        | AppPackageStagingError::CommitStateUnknown
        | AppPackageStagingError::Io(_) => AppCompositionRetryClass::Transient,
        AppPackageStagingError::Authentication(_)
        | AppPackageStagingError::Manifest(_)
        | AppPackageStagingError::UnsupportedPlatform
        | AppPackageStagingError::UnsafeSource(_)
        | AppPackageStagingError::UnsafeDestination(_)
        | AppPackageStagingError::ContentConflict
        | AppPackageStagingError::ScopeCollision
        | AppPackageStagingError::RecoveryLimitExceeded
        | AppPackageStagingError::Encoding(_) => AppCompositionRetryClass::Permanent,
    }
}

fn entity_store_retry_class(error: &AppEntityStoreError) -> AppCompositionRetryClass {
    match error {
        AppEntityStoreError::Registry(error) => registry_retry_class(error),
        AppEntityStoreError::Sqlite(_)
        | AppEntityStoreError::StaleSchemaBinding
        | AppEntityStoreError::StaleGrantBinding
        | AppEntityStoreError::CursorCapacityExceeded
        | AppEntityStoreError::CursorSnapshotUnavailable
        | AppEntityStoreError::StaleDatasetGeneration
        | AppEntityStoreError::StaleRecordProjection => AppCompositionRetryClass::Transient,
        AppEntityStoreError::Encoding(_)
        | AppEntityStoreError::Contract(_)
        | AppEntityStoreError::Schema(_)
        | AppEntityStoreError::Boundary(_)
        | AppEntityStoreError::Query(_)
        | AppEntityStoreError::InstallationNotEnabled(_)
        | AppEntityStoreError::MissingActiveSchemaRevision
        | AppEntityStoreError::MissingGrantRevision
        | AppEntityStoreError::MissingGrantRecord
        | AppEntityStoreError::MissingActiveSchemaRecord
        | AppEntityStoreError::MissingPackageRevision
        | AppEntityStoreError::PackageSchemaDigestMismatch
        | AppEntityStoreError::ScopeOrIdentityMismatch
        | AppEntityStoreError::InvalidRevision
        | AppEntityStoreError::MissingScopedStore
        | AppEntityStoreError::MissingInstallation
        | AppEntityStoreError::UnknownEntity(_)
        | AppEntityStoreError::UnknownField(_)
        | AppEntityStoreError::UnknownRelation(_)
        | AppEntityStoreError::InvalidPredicate
        | AppEntityStoreError::SnapshotTooLarge
        | AppEntityStoreError::QueryScanTooLarge
        | AppEntityStoreError::MissingCursor
        | AppEntityStoreError::CorruptCursor
        | AppEntityStoreError::CorruptDataset
        | AppEntityStoreError::CorruptRecord
        | AppEntityStoreError::SourceProjectionTooLarge
        | AppEntityStoreError::RelationProjectionTooLarge
        | AppEntityStoreError::EmptyProjection
        | AppEntityStoreError::PersonalAgentPolicyDenied
        | AppEntityStoreError::InvalidRecordProjection
        | AppEntityStoreError::RecipeRecordLocatorSubstitution
        | AppEntityStoreError::InvalidCursorExpiry
        // A behavior's source record being absent is not transient: retrying
        // cannot create it, and the behavior itself is forbidden from doing so.
        // Something else has to write the row.
        | AppEntityStoreError::MissingBehaviorSourceRecord
        | AppEntityStoreError::Index
        | AppEntityStoreError::KeysetIndexRequired => AppCompositionRetryClass::Permanent,
    }
}

fn entity_mutation_retry_class(error: &AppEntityMutationError) -> AppCompositionRetryClass {
    match error {
        AppEntityMutationError::Registry(error) => registry_retry_class(error),
        AppEntityMutationError::Store(error) => entity_store_retry_class(error),
        AppEntityMutationError::Sqlite(_)
        | AppEntityMutationError::SchemaRevisionConflict
        | AppEntityMutationError::RecordRevisionConflict
        | AppEntityMutationError::RelationRevisionConflict => AppCompositionRetryClass::Transient,
        _ => AppCompositionRetryClass::Permanent,
    }
}

fn entity_adapter_retry_class(error: &AppEntityAdapterError) -> AppCompositionRetryClass {
    match error {
        AppEntityAdapterError::Store(error) => entity_store_retry_class(error),
        AppEntityAdapterError::Mutation(error) => entity_mutation_retry_class(error),
        AppEntityAdapterError::StaleSurfaceRevision
        | AppEntityAdapterError::StaleWorkflowBinding => AppCompositionRetryClass::Transient,
        AppEntityAdapterError::Boundary(_)
        | AppEntityAdapterError::Contract(_)
        | AppEntityAdapterError::Json(_)
        | AppEntityAdapterError::MissingInstallation
        | AppEntityAdapterError::PersonalAgentProjectionDenied
        | AppEntityAdapterError::PersonalAgentSearchDenied
        | AppEntityAdapterError::PersonalAgentFieldDenied(_)
        | AppEntityAdapterError::UnknownEntity(_)
        | AppEntityAdapterError::UnknownRelation(_) => AppCompositionRetryClass::Permanent,
    }
}

fn resource_retry_class(error: &AppResourceAuthorityError) -> AppCompositionRetryClass {
    match error {
        AppResourceAuthorityError::Registry(error) => registry_retry_class(error),
        AppResourceAuthorityError::Package(error) => staging_retry_class(error),
        AppResourceAuthorityError::Sqlite(_)
        | AppResourceAuthorityError::StaleRegistryAuthority(_)
        | AppResourceAuthorityError::JournalRevisionConflict { .. }
        | AppResourceAuthorityError::PeriodRevisionConflict { .. }
        | AppResourceAuthorityError::SchedulerCapacityUnavailable => {
            AppCompositionRetryClass::Transient
        },
        _ => AppCompositionRetryClass::Permanent,
    }
}

fn artifact_retry_class(error: &ArtifactV2Error) -> AppCompositionRetryClass {
    match error {
        ArtifactV2Error::Io(_) => AppCompositionRetryClass::Transient,
        ArtifactV2Error::CommitUncertain { .. } => AppCompositionRetryClass::EffectUncertain,
        ArtifactV2Error::Serde(_)
        | ArtifactV2Error::TaskNotFound(_)
        | ArtifactV2Error::TaskPlanNotFound(_)
        | ArtifactV2Error::ExecutionNotFound(_)
        | ArtifactV2Error::InvalidRequest(_)
        | ArtifactV2Error::Runtime(_)
        | ArtifactV2Error::AlreadyResolved(_) => AppCompositionRetryClass::Permanent,
    }
}

fn workflow_retry_class(error: &AppWorkflowError) -> AppCompositionRetryClass {
    match error {
        AppWorkflowError::LaunchCancelledBeforeDispatch => AppCompositionRetryClass::Cancelled,
        AppWorkflowError::Registry(error) => registry_retry_class(error),
        AppWorkflowError::Staging(error) => staging_retry_class(error),
        AppWorkflowError::Store(error) => entity_store_retry_class(error),
        AppWorkflowError::Adapter(error) => entity_adapter_retry_class(error),
        AppWorkflowError::Resource(error) => resource_retry_class(error),
        AppWorkflowError::Artifact(error) => artifact_retry_class(error),
        AppWorkflowError::WorkerTerminated(_)
        | AppWorkflowError::StaleInvocation(_)
        | AppWorkflowError::StaleWorkflowPersonality
        | AppWorkflowError::StaleRuntimeAuthority => AppCompositionRetryClass::Transient,
        _ => AppCompositionRetryClass::Permanent,
    }
}

fn mapping_target(operation: &AppValueMappingOperation) -> &AppFieldPath {
    match operation {
        AppValueMappingOperation::Select { target, .. }
        | AppValueMappingOperation::Constant { target, .. }
        | AppValueMappingOperation::Convert { target, .. }
        | AppValueMappingOperation::MapEnum { target, .. } => target,
    }
}

fn validate_mapping_transport_bounds(
    mapping: &[AppValueMappingOperation],
) -> Result<(), AppComposeInvokeError> {
    let limits = AppContractLimits::default();
    if mapping.is_empty() || mapping.len() > limits.max_collection_items() {
        return Err(AppValueMappingError::OperationLimit {
            limit: limits.max_collection_items(),
        }
        .into());
    }
    for operation in mapping {
        match operation {
            AppValueMappingOperation::Constant { value, .. } => {
                validate_json_value(value, &limits)?;
            },
            AppValueMappingOperation::MapEnum { values, .. }
                if values.len() > limits.max_collection_items() =>
            {
                return Err(AppValueMappingError::InvalidEnumMapping.into());
            },
            AppValueMappingOperation::Select { .. }
            | AppValueMappingOperation::Convert { .. }
            | AppValueMappingOperation::MapEnum { .. } => {},
        }
    }
    Ok(())
}

fn canonical_mapping(
    mut mapping: Vec<AppValueMappingOperation>,
) -> Result<Vec<AppValueMappingOperation>, AppComposeInvokeError> {
    validate_mapping_transport_bounds(&mapping)?;
    mapping.sort_by(|left, right| mapping_target(left).cmp(mapping_target(right)));
    Ok(mapping)
}

fn mapping_request_digest(
    mapping: &[AppValueMappingOperation],
) -> Result<AppDigest, AppComposeInvokeError> {
    Ok(AppDigest::blake3_canonical_json(&serde_json::to_value(
        mapping,
    )?)?)
}

fn record_source_request_digest(
    source_projection_handle: &AppReference,
    source_record_id: Option<&AppRecordId>,
) -> Result<AppDigest, AppComposeInvokeError> {
    Ok(AppDigest::blake3_canonical_json(&serde_json::json!({
        "kind": "entity_record",
        "source_projection_handle": source_projection_handle,
        "source_record_id": source_record_id,
    }))?)
}

fn action_source_request_digest(
    source_run_ref: &AppReference,
) -> Result<AppDigest, AppComposeInvokeError> {
    Ok(AppDigest::blake3_canonical_json(&serde_json::json!({
        "kind": "action_result",
        "source_run_ref": source_run_ref,
    }))?)
}

fn action_chain_source_request_digest(
    source_run_ref: &AppReference,
    chain_request_digest: &AppDigest,
    hop_index: u8,
    hop_count: u8,
) -> Result<AppDigest, AppComposeInvokeError> {
    Ok(AppDigest::blake3_canonical_json(&serde_json::json!({
        "kind": "action_result_chain",
        "source_run_ref": source_run_ref,
        "chain_request_digest": chain_request_digest,
        "hop_index": hop_index,
        "hop_count": hop_count,
    }))?)
}

#[allow(clippy::too_many_arguments)]
fn build_action_composition_response(
    authenticated: &AuthenticatedAppScope,
    origin_source_run_ref: &AppReference,
    chain_request_digest: &AppDigest,
    hop_index: u8,
    hop_count: u8,
    destination_installation_id: &AppInstallationId,
    destination_action_id: &AppName,
    result: AppActionResultComposition,
    subscription_request: Option<&AppActionCompositionSubscriptionRequest>,
    now: DateTime<Utc>,
) -> Result<AppActionCompositionResponse, AppComposeInvokeError> {
    let (active_source_run_ref, status, destination_run_ref, state_rank) = match &result {
        AppActionResultComposition::Waiting { source_run } => (
            source_run.run_ref.clone(),
            AppActionCompositionUpdateStatus::Waiting,
            None,
            1_u64,
        ),
        AppActionResultComposition::Launched {
            source_run, launch, ..
        } => (
            source_run.run_ref.clone(),
            AppActionCompositionUpdateStatus::Launched,
            Some(launch.run_handle.run_ref.clone()),
            if launch.result.as_ref().is_some_and(|result| {
                matches!(result.status, super::models::AppActionStatus::Completed)
            }) {
                4
            } else {
                3
            },
        ),
        AppActionResultComposition::SourceTerminal { source_run, .. } => (
            source_run.run_ref.clone(),
            AppActionCompositionUpdateStatus::SourceTerminal,
            None,
            4,
        ),
        AppActionResultComposition::Unavailable {
            source_run,
            retry_class,
            ..
        } => (
            source_run.run_ref.clone(),
            AppActionCompositionUpdateStatus::Unavailable,
            None,
            match retry_class {
                AppCompositionRetryClass::Transient => 2,
                AppCompositionRetryClass::EffectUncertain => 3,
                AppCompositionRetryClass::Permanent | AppCompositionRetryClass::Cancelled => 4,
            },
        ),
    };
    let current_sequence = u64::from(hop_index)
        .checked_mul(4)
        .and_then(|base| base.checked_add(state_rank))
        .ok_or(AppContractError::InvalidField {
            field: "chain",
            message: "composition sequence overflow".to_owned(),
        })?;
    let chain = AppActionCompositionChainProgress {
        origin_source_run_ref: origin_source_run_ref.clone(),
        active_source_run_ref: active_source_run_ref.clone(),
        active_destination_installation_id: destination_installation_id.clone(),
        active_destination_action_id: destination_action_id.clone(),
        hop_index,
        hop_count,
    };
    let subscription = subscription_request
        .map(|request| {
            build_action_composition_subscription_page(
                authenticated,
                origin_source_run_ref,
                chain_request_digest,
                &chain,
                status,
                destination_run_ref,
                current_sequence,
                request,
                now,
            )
        })
        .transpose()?;
    Ok(AppActionCompositionResponse {
        result,
        chain,
        subscription,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_action_composition_subscription_page(
    authenticated: &AuthenticatedAppScope,
    origin_source_run_ref: &AppReference,
    chain_request_digest: &AppDigest,
    chain: &AppActionCompositionChainProgress,
    status: AppActionCompositionUpdateStatus,
    destination_run_ref: Option<AppReference>,
    current_sequence: u64,
    request: &AppActionCompositionSubscriptionRequest,
    now: DateTime<Utc>,
) -> Result<AppActionCompositionSubscriptionPage, AppComposeInvokeError> {
    let expires_at =
        now.checked_add_signed(Duration::minutes(10))
            .ok_or(AppContractError::InvalidField {
                field: "subscription",
                message: "cursor expiry overflow".to_owned(),
            })?;
    let mut after_sequence = 0;
    let mut reset_required = false;
    let had_cursor = request.cursor.is_some();
    if let Some(cursor) = request.cursor.as_ref() {
        match decode_action_composition_cursor(
            authenticated,
            origin_source_run_ref,
            chain_request_digest,
            cursor,
            now,
        ) {
            Ok(sequence) if sequence <= current_sequence => after_sequence = sequence,
            Ok(_) => {
                return Err(AppContractError::InvalidField {
                    field: "subscription.cursor",
                    message: "is ahead of the canonical composition sequence".to_owned(),
                }
                .into());
            },
            Err(AppCompositionCursorError::Expired) => reset_required = true,
            Err(AppCompositionCursorError::Invalid) => {
                return Err(AppContractError::InvalidField {
                    field: "subscription.cursor",
                    message: "does not belong to this scope, run, or exact chain request"
                        .to_owned(),
                }
                .into());
            },
        }
    }
    if had_cursor && current_sequence.saturating_sub(after_sequence) > 1 {
        // The public owner retains only the current payload-free state. A
        // client which missed an intermediate transition receives an explicit
        // reset instead of an invented contiguous history.
        reset_required = true;
    }
    let updates = if current_sequence > after_sequence || reset_required {
        vec![AppActionCompositionUpdate {
            sequence: current_sequence,
            source_run_ref: chain.active_source_run_ref.clone(),
            destination_installation_id: chain.active_destination_installation_id.clone(),
            destination_action_id: chain.active_destination_action_id.clone(),
            status,
            destination_run_ref,
            observed_at: now,
        }]
    } else {
        Vec::new()
    };
    let next_cursor = encode_action_composition_cursor(
        authenticated,
        origin_source_run_ref,
        chain_request_digest,
        current_sequence,
        expires_at,
    )?;
    Ok(AppActionCompositionSubscriptionPage {
        after_sequence,
        through_sequence: updates
            .last()
            .map_or(after_sequence, |update| update.sequence),
        current_sequence,
        updates,
        has_more: false,
        reset_required,
        next_cursor,
        expires_at,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppCompositionCursorError {
    Invalid,
    Expired,
}

fn action_composition_cursor_digest(
    authenticated: &AuthenticatedAppScope,
    origin_source_run_ref: &AppReference,
    chain_request_digest: &AppDigest,
    sequence: u64,
    expires_at_ms: i64,
) -> Result<AppDigest, AppComposeInvokeError> {
    Ok(AppDigest::blake3_canonical_json(&serde_json::json!({
        "scope_binding_ref": authenticated.scope_binding_ref(),
        "authentication_revision": authenticated.authentication_revision(),
        "origin_source_run_ref": origin_source_run_ref,
        "chain_request_digest": chain_request_digest,
        "sequence": sequence,
        "expires_at_ms": expires_at_ms,
    }))?)
}

fn encode_action_composition_cursor(
    authenticated: &AuthenticatedAppScope,
    origin_source_run_ref: &AppReference,
    chain_request_digest: &AppDigest,
    sequence: u64,
    expires_at: DateTime<Utc>,
) -> Result<AppReference, AppComposeInvokeError> {
    let expires_at_ms = expires_at.timestamp_millis();
    let digest = action_composition_cursor_digest(
        authenticated,
        origin_source_run_ref,
        chain_request_digest,
        sequence,
        expires_at_ms,
    )?;
    AppReference::parse(format!(
        "composition-cursor:{sequence}:{expires_at_ms}:{}",
        digest.as_str().trim_start_matches("blake3:")
    ))
    .map_err(Into::into)
}

fn decode_action_composition_cursor(
    authenticated: &AuthenticatedAppScope,
    origin_source_run_ref: &AppReference,
    chain_request_digest: &AppDigest,
    cursor: &AppReference,
    now: DateTime<Utc>,
) -> Result<u64, AppCompositionCursorError> {
    let mut parts = cursor.as_str().split(':');
    if parts.next() != Some("composition-cursor") {
        return Err(AppCompositionCursorError::Invalid);
    }
    let sequence = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .ok_or(AppCompositionCursorError::Invalid)?;
    let expires_at_ms = parts
        .next()
        .and_then(|part| part.parse::<i64>().ok())
        .ok_or(AppCompositionCursorError::Invalid)?;
    let supplied_digest = parts.next().ok_or(AppCompositionCursorError::Invalid)?;
    if parts.next().is_some() {
        return Err(AppCompositionCursorError::Invalid);
    }
    if now.timestamp_millis() >= expires_at_ms {
        return Err(AppCompositionCursorError::Expired);
    }
    let expected = action_composition_cursor_digest(
        authenticated,
        origin_source_run_ref,
        chain_request_digest,
        sequence,
        expires_at_ms,
    )
    .map_err(|_| AppCompositionCursorError::Invalid)?;
    if expected.as_str().trim_start_matches("blake3:") != supplied_digest {
        return Err(AppCompositionCursorError::Invalid);
    }
    Ok(sequence)
}

fn canonical_action_source_task_id(source_run_ref: &AppReference) -> Option<&str> {
    let task_id = source_run_ref
        .as_str()
        .strip_prefix(APP_ACTION_RUN_REF_PREFIX)?;
    is_canonical_app_workflow_task_id(task_id).then_some(task_id)
}

fn select_source_projection<'a>(
    page: &'a AppQueryPage,
    record_id: Option<&AppRecordId>,
) -> Result<&'a AppRecordProjection, AppComposeInvokeError> {
    match record_id {
        Some(record_id) => {
            let mut matching = page
                .envelope
                .value
                .iter()
                .filter(|projection| &projection.record_id == record_id);
            let projection = matching
                .next()
                .ok_or(AppComposeInvokeError::MissingSourceRecord)?;
            if matching.next().is_some() {
                return Err(AppComposeInvokeError::AmbiguousSourceRecord);
            }
            Ok(projection)
        },
        None => match page.envelope.value.as_slice() {
            [projection] => Ok(projection),
            [] => Err(AppComposeInvokeError::MissingSourceRecord),
            _ => Err(AppComposeInvokeError::SourceRecordRequired),
        },
    }
}

fn select_source_ref<'a>(
    page: &'a AppQueryPage,
    projection: &AppRecordProjection,
) -> Result<&'a AppSourceRef, AppComposeInvokeError> {
    let identity = serde_json::json!({
        "entity": projection.entity,
        "record_id": projection.record_id,
    });
    let digest = AppDigest::blake3_canonical_json(&identity)?;
    let reference = AppReference::parse(format!("record:{}", digest.as_str()))?;
    let mut matching = page.envelope.source_refs.iter().filter(|source| {
        source.kind == AppSourceRefKind::EntityField
            && source.reference == reference
            && source.revision == Some(projection.record_revision)
    });
    let source_ref = matching
        .next()
        .ok_or(AppComposeInvokeError::MissingSourceProvenance)?;
    if matching.next().is_some() {
        return Err(AppComposeInvokeError::AmbiguousSourceProvenance);
    }
    Ok(source_ref)
}

fn source_value_schema(
    source: &AppRevalidatedRecordProjection,
    selected_fields: &[AppFieldPath],
) -> Result<AppValueSchemaContract, AppComposeInvokeError> {
    let runtime = source
        .active()
        .runtime_contract(&source.projection().entity)
        .ok_or(AppComposeInvokeError::MissingSourceContract)?;
    let mut fields = BTreeMap::new();
    for path in selected_fields {
        let field = runtime
            .field(path)
            .ok_or_else(|| AppComposeInvokeError::MissingSourceField(path.to_string()))?;
        fields.insert(
            path.clone(),
            AppValueFieldContract {
                kind: field.kind(),
                required: field.required(),
                nullable: field.nullable(),
                enum_values: field.enum_values().clone(),
            },
        );
    }
    Ok(AppValueSchemaContract::from_compiled_fields(fields)?)
}

fn source_value_schema_from_manifest_output(
    schema: &AppManifestInputSchema,
) -> Result<AppValueSchemaContract, AppComposeInvokeError> {
    let compiled = schema.compiled_value_schema()?;
    workflow_value_mapping_schema(&compiled).map_err(Into::into)
}

fn source_envelope(
    authenticated: &AuthenticatedAppScope,
    source: &AppRevalidatedRecordProjection,
    schema: &AppValueSchemaContract,
    now: DateTime<Utc>,
) -> Result<AppDataEnvelope<Value>, AppComposeInvokeError> {
    let mut object = Map::new();
    for (path, value) in &source.projection().fields {
        write_source_path(&mut object, path, value.clone())?;
    }
    let value = Value::Object(object);
    let source_refs = vec![source.source_ref().clone()];
    let handling_labels = AppHandlingLabels {
        classification: source.handling_policy().classification_floor,
        model_processing: source.handling_policy().model_processing,
        policy_digest: AppDigest::blake3_canonical_json(&serde_json::to_value(
            source.handling_policy(),
        )?)?,
        provenance_digest: AppDigest::blake3_canonical_json(&serde_json::to_value(&source_refs)?)?,
    };
    let envelope = AppDataEnvelope {
        protocol_version: AppProtocolVersion::V1,
        source: AppDataSource::AppStore,
        scope_binding_ref: authenticated.scope_binding_ref().clone(),
        installation_id: source.active().installation_id().clone(),
        package_revision_ref: source.active().package_revision_ref().clone(),
        schema_revision: source.active().schema_revision(),
        grant_revision: source.active().grant_revision(),
        value_schema_ref: schema.schema_ref().clone(),
        content_digest: AppDigest::blake3_canonical_json(&value)?,
        value,
        source_refs,
        handling_labels,
        produced_at: now,
        expires_at: Some(
            now.checked_add_signed(Duration::minutes(10))
                .ok_or(AppComposeInvokeError::ClockOverflow)?,
        ),
    };
    envelope.validate_app_contract(&AppContractLimits::default())?;
    Ok(envelope)
}

fn write_source_path(
    root: &mut Map<String, Value>,
    path: &AppFieldPath,
    value: Value,
) -> Result<(), AppComposeInvokeError> {
    let mut segments = path.as_str().split('.').peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current.insert(segment.to_owned(), value);
            return Ok(());
        }
        let child = current
            .entry(segment.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        current = child
            .as_object_mut()
            .ok_or_else(|| AppComposeInvokeError::SourceShapeConflict(path.to_string()))?;
    }
    Err(AppComposeInvokeError::SourceShapeConflict(path.to_string()))
}

fn value_schema_from_manifest_input(
    input: &AppManifestInputSchema,
) -> Result<AppValueSchemaContract, AppComposeInvokeError> {
    let compiled = input.compiled_value_schema()?;
    workflow_value_mapping_schema(&compiled).map_err(Into::into)
}

fn transfer_id(
    idempotency_key: &AppReference,
    source: &AppDataEnvelope<Value>,
    source_installation_generation: u64,
    destination: &AppCompositionDestination,
    mapping: &[AppValueMappingOperation],
) -> Result<AppReference, AppComposeInvokeError> {
    #[derive(Serialize)]
    struct TransferIdentity<'a> {
        idempotency_key: &'a AppReference,
        source_installation_id: &'a AppInstallationId,
        source_installation_generation: u64,
        source_package_revision_ref: &'a AppReference,
        source_content_digest: &'a AppDigest,
        source_handling_policy_digest: &'a AppDigest,
        source_provenance_digest: &'a AppDigest,
        source_schema_revision: AppRevision,
        source_grant_revision: AppRevision,
        destination_installation_id: &'a AppInstallationId,
        destination_package_revision_ref: &'a AppReference,
        destination_schema_revision: AppRevision,
        destination_grant_revision: AppRevision,
        destination_action_id: &'a AppName,
        destination_action_revision: AppRevision,
        mapping: &'a [AppValueMappingOperation],
    }
    let digest = AppDigest::blake3_canonical_json(&serde_json::to_value(TransferIdentity {
        idempotency_key,
        source_installation_id: &source.installation_id,
        source_installation_generation,
        source_package_revision_ref: &source.package_revision_ref,
        source_content_digest: &source.content_digest,
        source_handling_policy_digest: &source.handling_labels.policy_digest,
        source_provenance_digest: &source.handling_labels.provenance_digest,
        source_schema_revision: source.schema_revision,
        source_grant_revision: source.grant_revision,
        destination_installation_id: &destination.installation_id,
        destination_package_revision_ref: &destination.package_revision_ref,
        destination_schema_revision: destination.schema_revision,
        destination_grant_revision: destination.grant_revision,
        destination_action_id: &destination.action_id,
        destination_action_revision: destination.action_revision,
        mapping,
    })?)?;
    Ok(AppReference::parse(format!(
        "transfer:{}",
        digest.as_str()
    ))?)
}

#[derive(Debug, Error)]
pub enum AppComposeInvokeError {
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Manifest(#[from] AppManifestError),
    #[error(transparent)]
    Boundary(#[from] super::boundary::AppBoundaryError),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
    #[error(transparent)]
    Mapping(#[from] AppValueMappingError),
    #[error(transparent)]
    WorkflowSchema(#[from] super::recipe_ir::AppRecipeIrError),
    #[error(transparent)]
    Composition(#[from] AppCompositionError),
    #[error(transparent)]
    Entity(#[from] AppEntityAdapterError),
    #[error(transparent)]
    EntityStore(#[from] super::entity_store::AppEntityStoreError),
    #[error(transparent)]
    Registry(#[from] AppRegistryError),
    #[error(transparent)]
    Staging(#[from] AppPackageStagingError),
    #[error(transparent)]
    Workflow(#[from] AppWorkflowError),
    #[error("app composition clock overflowed")]
    ClockOverflow,
    #[error("source query page contains no selected record")]
    MissingSourceRecord,
    #[error("source query page contains duplicate selected record identities")]
    AmbiguousSourceRecord,
    #[error("source_record_id is required when the query page has multiple rows")]
    SourceRecordRequired,
    #[error("source query page does not contain exact record provenance")]
    MissingSourceProvenance,
    #[error("source query page contains ambiguous record provenance")]
    AmbiguousSourceProvenance,
    #[error("source record contract is unavailable")]
    MissingSourceContract,
    #[error("source action result schema contains no fields to compose")]
    EmptySourceResult,
    #[error("source record contract does not contain selected field `{0}`")]
    MissingSourceField(String),
    #[error("source record field `{0}` conflicts with another projected field path")]
    SourceShapeConflict(String),
    #[error("V1 app composition does not admit relation-expanded source projections")]
    UnsupportedSourceRelations,
    #[error("destination installation does not exist")]
    MissingDestinationInstallation,
    #[error("destination installation is not enabled")]
    DestinationUnavailable,
    #[error("destination installation changed while composition was being resolved")]
    DestinationChanged,
    #[error("destination package revision does not exist")]
    MissingDestinationPackage,
    #[error("destination action does not exist")]
    MissingDestinationAction,
    #[error("destination action workflow does not exist")]
    MissingDestinationWorkflow,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn authenticated_scope(now: DateTime<Utc>) -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_verified_session(
            super::super::records::AppScope {
                principal: AppReference::parse("principal:owner").unwrap(),
                workspace: AppReference::parse("workspace:default").unwrap(),
            },
            super::super::models::AppScopeBindingRef::parse("scope_owner_default").unwrap(),
            AppReference::parse("actor:owner").unwrap(),
            AppReference::parse("session:owner").unwrap(),
            AppRevision::new(7).unwrap(),
            now - Duration::minutes(1),
            now + Duration::minutes(30),
        )
        .unwrap()
    }

    fn run_handle() -> super::super::models::AppRunHandle {
        super::super::models::AppRunHandle {
            protocol_version: AppProtocolVersion::V1,
            run_ref: AppReference::parse("run:app-action:task_app_source").unwrap(),
            installation_id: AppInstallationId::parse("install_source").unwrap(),
            action_id: AppName::parse("source_action").unwrap(),
        }
    }

    fn select(source: &str, target: &str) -> AppValueMappingOperation {
        AppValueMappingOperation::Select {
            source: AppFieldPath::parse(source).unwrap(),
            target: AppFieldPath::parse(target).unwrap(),
        }
    }

    #[test]
    fn mapping_identity_order_is_canonical_before_transfer_hashing() {
        let forward = canonical_mapping(vec![select("a", "first"), select("b", "second")]).unwrap();
        let reverse = canonical_mapping(vec![select("b", "second"), select("a", "first")]).unwrap();
        assert_eq!(forward, reverse);
        assert_eq!(
            mapping_request_digest(&forward).unwrap(),
            mapping_request_digest(&reverse).unwrap()
        );
    }

    #[test]
    fn record_recovery_identity_binds_exact_handle_and_optional_record() {
        let first_handle = AppReference::parse("projection:first").unwrap();
        let second_handle = AppReference::parse("projection:second").unwrap();
        let record = AppRecordId::parse("record-a").unwrap();

        let first = record_source_request_digest(&first_handle, Some(&record)).unwrap();
        assert_eq!(
            first,
            record_source_request_digest(&first_handle, Some(&record)).unwrap()
        );
        assert_ne!(
            first,
            record_source_request_digest(&second_handle, Some(&record)).unwrap()
        );
        assert_ne!(
            first,
            record_source_request_digest(&first_handle, None).unwrap()
        );
    }

    #[test]
    fn action_recovery_identity_binds_canonical_run_ref() {
        let first =
            AppReference::parse(format!("run:app-action:task_app_{}", "a".repeat(64))).unwrap();
        let second =
            AppReference::parse(format!("run:app-action:task_app_{}", "b".repeat(64))).unwrap();

        assert_eq!(
            action_source_request_digest(&first).unwrap(),
            action_source_request_digest(&first).unwrap()
        );
        assert_ne!(
            action_source_request_digest(&first).unwrap(),
            action_source_request_digest(&second).unwrap()
        );
        assert!(canonical_action_source_task_id(&first).is_some());
        assert!(canonical_action_source_task_id(
            &AppReference::parse(format!("task_app_{}", "a".repeat(64))).unwrap()
        )
        .is_none());
        assert!(canonical_action_source_task_id(
            &AppReference::parse("run:app-action:task_app_short").unwrap()
        )
        .is_none());
        let chain = AppDigest::blake3(b"chain");
        assert_ne!(
            action_chain_source_request_digest(&first, &chain, 0, 2).unwrap(),
            action_chain_source_request_digest(&first, &chain, 1, 2).unwrap(),
        );
    }

    #[test]
    fn public_chain_is_bounded_and_requires_unique_hop_keys() {
        let hop = AppActionCompositionHop {
            destination_installation_id: AppInstallationId::parse("install_destination").unwrap(),
            destination_action_id: AppName::parse("consume").unwrap(),
            mapping: vec![select("plan", "plan")],
            idempotency_key: AppReference::parse("compose:shared").unwrap(),
        };
        let duplicate = AppActionCompositionRequest {
            destination_installation_id: AppInstallationId::parse("install_first").unwrap(),
            destination_action_id: AppName::parse("consume").unwrap(),
            mapping: vec![select("plan", "plan")],
            idempotency_key: AppReference::parse("compose:shared").unwrap(),
            chain: vec![hop.clone()],
            subscription: None,
        };
        assert!(duplicate
            .validate_app_contract(&AppContractLimits::default())
            .is_err());

        let too_deep = AppActionCompositionRequest {
            idempotency_key: AppReference::parse("compose:first").unwrap(),
            chain: vec![
                AppActionCompositionHop {
                    idempotency_key: AppReference::parse("compose:second").unwrap(),
                    ..hop.clone()
                },
                AppActionCompositionHop {
                    idempotency_key: AppReference::parse("compose:third").unwrap(),
                    ..hop.clone()
                },
                AppActionCompositionHop {
                    idempotency_key: AppReference::parse("compose:fourth").unwrap(),
                    ..hop
                },
            ],
            ..duplicate
        };
        assert!(too_deep
            .validate_app_contract(&AppContractLimits::default())
            .is_err());
    }

    #[test]
    fn subscription_cursor_binds_scope_run_chain_sequence_and_expiry() {
        let now = "2026-08-23T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let authenticated = authenticated_scope(now);
        let source = AppReference::parse("run:app-action:task_app_source").unwrap();
        let other_source = AppReference::parse("run:app-action:task_app_other").unwrap();
        let chain = AppDigest::blake3(b"chain-a");
        let other_chain = AppDigest::blake3(b"chain-b");
        let expires_at = now + Duration::minutes(10);
        let cursor =
            encode_action_composition_cursor(&authenticated, &source, &chain, 5, expires_at)
                .unwrap();

        assert_eq!(
            decode_action_composition_cursor(&authenticated, &source, &chain, &cursor, now,),
            Ok(5),
        );
        assert_eq!(
            decode_action_composition_cursor(&authenticated, &other_source, &chain, &cursor, now,),
            Err(AppCompositionCursorError::Invalid),
        );
        assert_eq!(
            decode_action_composition_cursor(&authenticated, &source, &other_chain, &cursor, now,),
            Err(AppCompositionCursorError::Invalid),
        );
        assert_eq!(
            decode_action_composition_cursor(&authenticated, &source, &chain, &cursor, expires_at,),
            Err(AppCompositionCursorError::Expired),
        );
    }

    #[test]
    fn brokered_launch_failures_keep_the_dispatch_stage() {
        assert_eq!(
            brokered_launch_retry_class(&AppBrokeredWorkflowLaunchError::CancelledBeforeDispatch),
            AppCompositionRetryClass::Cancelled
        );
        assert_eq!(
            brokered_launch_retry_class(&AppBrokeredWorkflowLaunchError::PreDispatch(
                AppWorkflowError::WorkerTerminated("stopped".to_owned()),
            )),
            AppCompositionRetryClass::Transient
        );
        assert_eq!(
            brokered_launch_retry_class(&AppBrokeredWorkflowLaunchError::EffectUncertain(
                AppWorkflowError::CorruptBinding,
            )),
            AppCompositionRetryClass::EffectUncertain
        );
    }

    #[test]
    fn public_composition_projection_omits_internal_transfer_receipt() {
        let launch = AppCompositionOutcome::Launched {
            launch: AppCompositionWorkflowLaunch::from(AppWorkflowLaunch {
                run_handle: run_handle(),
                task_id: "task_app_destination".to_owned(),
                execution_id: None,
                result: None,
            }),
            result_withheld_by_policy: false,
        };

        let value = serde_json::to_value(launch).unwrap();
        assert!(value.get("transfer_receipt").is_none());
        assert!(value["launch"].get("task_id").is_none());
        assert!(value["launch"].get("execution_id").is_none());
        assert_eq!(
            value["launch"]["run_handle"]["run_ref"],
            "run:app-action:task_app_source"
        );
    }

    #[test]
    fn model_result_projection_strips_labels_provenance_and_exact_receipts() {
        let output_value = serde_json::json!({"summary": "done"});
        let launch = AppCompositionWorkflowLaunch::from(AppWorkflowLaunch {
            run_handle: run_handle(),
            task_id: "task_app_destination".to_owned(),
            execution_id: Some("execution:destination".to_owned()),
            result: Some(super::super::models::AppActionResult {
                protocol_version: AppProtocolVersion::V1,
                action_id: AppName::parse("destination_action").unwrap(),
                run_ref: AppReference::parse("run:app-action:task_app_destination").unwrap(),
                status: super::super::models::AppActionStatus::Completed,
                output: Some(AppDataEnvelope {
                    protocol_version: AppProtocolVersion::V1,
                    source: AppDataSource::AppAction,
                    scope_binding_ref: super::super::models::AppScopeBindingRef::parse(
                        "scope_anonymous_default",
                    )
                    .unwrap(),
                    installation_id: AppInstallationId::parse("install_destination").unwrap(),
                    package_revision_ref: AppReference::parse("package:destination").unwrap(),
                    schema_revision: AppRevision::new(1).unwrap(),
                    grant_revision: AppRevision::new(1).unwrap(),
                    value_schema_ref: AppReference::parse("schema:result").unwrap(),
                    value: output_value.clone(),
                    source_refs: vec![AppSourceRef {
                        kind: AppSourceRefKind::EntityField,
                        reference: AppReference::parse("record:private").unwrap(),
                        revision: Some(AppRevision::new(1).unwrap()),
                        fields: vec![AppFieldPath::parse("summary").unwrap()],
                    }],
                    handling_labels: AppHandlingLabels {
                        classification: AppDataClassification::Secret,
                        model_processing: AppModelProcessing::LocalOnly,
                        policy_digest: AppDigest::blake3(b"private-policy"),
                        provenance_digest: AppDigest::blake3(b"private-provenance"),
                    },
                    content_digest: AppDigest::blake3_canonical_json(&output_value).unwrap(),
                    produced_at: "2026-08-21T00:00:00Z".parse().unwrap(),
                    expires_at: None,
                }),
                mutation_receipt_refs: vec![AppReference::parse("mutation:private").unwrap()],
                external_effect_receipt_refs: vec![AppReference::parse("receipt:private").unwrap()],
                error: None,
            }),
        });

        let value = serde_json::to_value(launch).unwrap();
        assert!(value.get("task_id").is_none());
        assert!(value.get("execution_id").is_none());
        assert_eq!(value["result"]["output"], output_value);
        assert_eq!(value["result"]["effect_committed"], true);
        assert!(value["result"].get("source_refs").is_none());
        assert!(value["result"].get("handling_labels").is_none());
        assert!(value["result"].get("mutation_receipt_refs").is_none());
        assert!(value["result"]
            .get("external_effect_receipt_refs")
            .is_none());
    }

    #[test]
    fn governed_composition_failure_has_one_stable_public_code() {
        let value = serde_json::to_value(AppActionResultComposition::Unavailable {
            source_run: run_handle(),
            error_code: AppActionCompositionErrorCode::OutcomeUnavailable,
            retry_class: AppCompositionRetryClass::Transient,
            retryable: true,
            effect_uncertain: false,
        })
        .unwrap();

        assert_eq!(value["status"], "unavailable");
        assert_eq!(value["error_code"], "outcome_unavailable");
        assert_eq!(value["retry_class"], "transient");
        assert_eq!(value["retryable"], true);
        assert_eq!(value["effect_uncertain"], false);
        assert!(value.get("reason").is_none());
        assert!(value.get("transfer_receipt").is_none());
    }

    #[test]
    fn reviewed_result_schema_retains_declared_optional_nullable_and_enum_contracts() {
        let schema: AppManifestInputSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "fields": {
                "note": {"type": "text"},
                "status": {
                    "type": "enum",
                    "values": ["red", "blue"],
                    "nullable": true
                }
            }
        }))
        .unwrap();
        let contract = source_value_schema_from_manifest_output(&schema).unwrap();

        let note = contract
            .field(&AppFieldPath::parse("note").unwrap())
            .unwrap();
        assert!(!note.required);
        assert!(!note.nullable);
        let status = contract
            .field(&AppFieldPath::parse("status").unwrap())
            .unwrap();
        assert!(!status.required);
        assert!(status.nullable);
        assert_eq!(
            status.enum_values,
            [
                AppName::parse("blue").unwrap(),
                AppName::parse("red").unwrap()
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn local_only_result_is_never_returned_to_remote_calling_model() {
        let remote = AppStoreReadAudience::PersonalAgent {
            execution_ref: AppReference::parse("execution:remote").unwrap(),
            processing_class: super::super::boundary::AppAgentProcessingClass::RemoteModel,
            maximum_classification: AppDataClassification::Secret,
        };
        let local = AppStoreReadAudience::PersonalAgent {
            execution_ref: AppReference::parse("execution:local").unwrap(),
            processing_class: super::super::boundary::AppAgentProcessingClass::LocalModel,
            maximum_classification: AppDataClassification::Secret,
        };
        assert!(!result_labels_permit_caller(
            &remote,
            AppDataClassification::Sensitive,
            AppModelProcessing::LocalOnly,
        ));
        assert!(result_labels_permit_caller(
            &local,
            AppDataClassification::Sensitive,
            AppModelProcessing::LocalOnly,
        ));
    }

    #[test]
    fn effect_only_result_is_label_neutral_for_caller_filtering() {
        let remote = AppStoreReadAudience::PersonalAgent {
            execution_ref: AppReference::parse("execution:remote").unwrap(),
            processing_class: super::super::boundary::AppAgentProcessingClass::RemoteModel,
            maximum_classification: AppDataClassification::Ordinary,
        };
        let result = AppActionResult {
            protocol_version: AppProtocolVersion::V1,
            action_id: AppName::parse("destination_action").unwrap(),
            run_ref: AppReference::parse("run:app-action:task_app_destination").unwrap(),
            status: super::super::models::AppActionStatus::Completed,
            output: None,
            mutation_receipt_refs: vec![AppReference::parse("mutation:private").unwrap()],
            external_effect_receipt_refs: Vec::new(),
            error: None,
        };

        assert!(brokered_result_labels_permit_caller(Some(&result), &remote));
    }

    #[test]
    fn retry_classification_separates_live_failures_from_invalid_or_corrupt_state() {
        assert_eq!(
            composition_retry_class(&AppComposeInvokeError::Registry(
                AppRegistryError::Overloaded,
            )),
            AppCompositionRetryClass::Transient,
        );
        assert_eq!(
            composition_retry_class(&AppComposeInvokeError::Registry(
                AppRegistryError::MissingRecord {
                    entity: "installation",
                    identity: "missing".to_owned(),
                },
            )),
            AppCompositionRetryClass::Permanent,
        );
        assert_eq!(
            composition_retry_class(&AppComposeInvokeError::Staging(
                AppPackageStagingError::SourceChanged,
            )),
            AppCompositionRetryClass::Transient,
        );
        assert_eq!(
            composition_retry_class(&AppComposeInvokeError::Staging(
                AppPackageStagingError::ContentConflict,
            )),
            AppCompositionRetryClass::Permanent,
        );
        assert_eq!(
            composition_retry_class(&AppComposeInvokeError::EntityStore(
                AppEntityStoreError::StaleRecordProjection,
            )),
            AppCompositionRetryClass::Transient,
        );
        assert_eq!(
            composition_retry_class(&AppComposeInvokeError::EntityStore(
                AppEntityStoreError::CorruptRecord,
            )),
            AppCompositionRetryClass::Permanent,
        );
        assert_eq!(
            composition_retry_class(&AppComposeInvokeError::Workflow(
                AppWorkflowError::StaleRuntimeAuthority,
            )),
            AppCompositionRetryClass::Transient,
        );
        assert_eq!(
            composition_retry_class(&AppComposeInvokeError::Workflow(
                AppWorkflowError::CorruptBinding,
            )),
            AppCompositionRetryClass::Permanent,
        );
    }
}
