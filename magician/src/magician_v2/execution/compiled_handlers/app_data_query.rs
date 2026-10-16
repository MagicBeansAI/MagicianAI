//! `app_data_query` — exact personal-agent query over one enabled installation.

use std::sync::Arc;

use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tracing::warn;

use super::app_data::{reattest_personal_agent_publication, require_direct_personal_agent_context};
use super::shared::require_scope_str;
use crate::magician_v2::apps::authority::AuthenticatedAppScope;
use crate::magician_v2::apps::boundary::{
    AppAgentProcessingClass, AppPersonalAgentPublicationFence,
};
use crate::magician_v2::apps::entity_adapter::AppGenericDataToolAdapter;
use crate::magician_v2::apps::models::{
    AppContractLimits, AppDataClassification, AppFieldPath, AppInstallationId, AppModelProcessing,
    AppName, AppPredicate, AppProtocolVersion, AppQueryOrder, AppQueryRequest, AppReference,
    AppRelationExpansion, ValidateAppContract,
};
use crate::magician_v2::apps::records::{
    AppDataHandlingPolicy, AppExternalEgress, AppMemoryPromotion, AppPersonalAgentAccess,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::compiled_dispatch::publish_current_compiled_app_result_guard;
use crate::magician_v2::execution::error::ExecutionError;
use crate::magician_v2::json_traversal::{exact_json_encoded_len, inspect_json_bounded};

pub(super) const APP_DATA_UNAVAILABLE_CODE: &str = "app_data_unavailable";

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AppDataUnavailableOutcome {
    protocol_version: AppProtocolVersion,
    status: &'static str,
    error_code: &'static str,
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    preflight_args(&args, "app_data_query")?;
    reject_unknown_args(
        &args,
        "app_data_query",
        &[
            "installation_id",
            "entity",
            "select",
            "predicate",
            "order",
            "cursor",
            "limit",
            "relation_expansions",
        ],
    )?;
    let select = parse_select(&args, "app_data_query", true)?.ok_or_else(|| {
        ExecutionError::Step("app_data_query requires a non-empty select array".into())
    })?;
    let predicate = parse_optional_typed(&args, "predicate", "app_data_query")?;
    let request = parse_query_request(
        &args,
        "app_data_query",
        select,
        predicate,
        "personal_agent_query",
    )?;
    let context =
        require_direct_personal_agent_context(Some(resources.as_ref()), &args, "app_data_query")?;
    let processing_class = context.authority.processing_class();
    let Some(artifact_service) = resources.artifact_v2_service.as_ref() else {
        warn!("app_data_query registry runtime is unavailable");
        return publish_app_data_unavailable(
            resources.as_ref(),
            &context.authenticated,
            &context.calling_profile_name,
            &context.publication_fence,
            processing_class,
            "app_data_query",
        );
    };
    let workflow = artifact_service.app_workflow_service();
    let adapter = AppGenericDataToolAdapter::new(workflow.registry_service());
    let execution_ref = context.authority.execution_ref().clone();
    let now = Utc::now();
    let governed = match adapter
        .query_governed(
            &context.authenticated,
            context.authority,
            request.clone(),
            now,
        )
        .await
    {
        Ok(governed) => governed,
        Err(error) => {
            warn!(error = ?error, "app_data_query governed read is unavailable");
            return publish_app_data_unavailable(
                resources.as_ref(),
                &context.authenticated,
                &context.calling_profile_name,
                &context.publication_fence,
                processing_class,
                "app_data_query",
            );
        },
    };
    let completion_now = Utc::now();
    reattest_personal_agent_publication(
        resources.as_ref(),
        &context.authenticated,
        &context.calling_profile_name,
        &context.publication_fence,
        "app_data_query",
        completion_now,
    )?;
    let (result, policy) = match workflow.register_personal_agent_projection(
        &context.authenticated,
        &execution_ref,
        request,
        governed,
        context.publication_fence.expires_at(),
        completion_now,
    ) {
        Ok(result) => result,
        Err(error) => {
            warn!(error = ?error, "app_data_query projection handle is unavailable");
            return publish_app_data_unavailable(
                resources.as_ref(),
                &context.authenticated,
                &context.calling_profile_name,
                &context.publication_fence,
                processing_class,
                "app_data_query",
            );
        },
    };
    let projection_handle = result.projection_handle.clone();
    let value = match serde_json::to_value(result) {
        Ok(value) => value,
        Err(error) => {
            warn!(error = ?error, "app_data_query result serialization is unavailable");
            return publish_app_data_unavailable(
                resources.as_ref(),
                &context.authenticated,
                &context.calling_profile_name,
                &context.publication_fence,
                processing_class,
                "app_data_query",
            );
        },
    };
    publish_current_compiled_app_result_guard(policy, &value, Some(projection_handle))?;
    Ok(value)
}

pub(super) fn publish_app_data_unavailable(
    resources: &AgentResources,
    authenticated: &AuthenticatedAppScope,
    profile_name: &str,
    fence: &AppPersonalAgentPublicationFence,
    processing_class: AppAgentProcessingClass,
    tool_name: &str,
) -> Result<Value, ExecutionError> {
    reattest_personal_agent_publication(
        resources,
        authenticated,
        profile_name,
        fence,
        tool_name,
        Utc::now(),
    )?;
    let value = serde_json::to_value(AppDataUnavailableOutcome {
        protocol_version: AppProtocolVersion::V1,
        status: "unavailable",
        error_code: APP_DATA_UNAVAILABLE_CODE,
    })
    .map_err(|_| ExecutionError::Step("app data unavailable".to_owned()))?;
    publish_current_compiled_app_result_guard(
        app_data_control_metadata_policy(processing_class),
        &value,
        None,
    )?;
    Ok(value)
}

fn app_data_control_metadata_policy(
    processing_class: AppAgentProcessingClass,
) -> AppDataHandlingPolicy {
    AppDataHandlingPolicy {
        classification_floor: AppDataClassification::Sensitive,
        model_processing: match processing_class {
            AppAgentProcessingClass::Deterministic => AppModelProcessing::None,
            AppAgentProcessingClass::LocalModel => AppModelProcessing::LocalOnly,
            AppAgentProcessingClass::RemoteModel => AppModelProcessing::RemoteAllowed,
        },
        personal_agent_access: AppPersonalAgentAccess::ApprovedProjection,
        memory_promotion: AppMemoryPromotion::Denied,
        external_egress: AppExternalEgress::Denied,
        approved_destinations: Vec::new(),
    }
}

pub(super) fn parse_query_request(
    args: &Value,
    tool_name: &str,
    select: Vec<AppFieldPath>,
    predicate: Option<AppPredicate>,
    default_purpose: &str,
) -> Result<AppQueryRequest, ExecutionError> {
    let installation_id =
        AppInstallationId::parse(require_scope_str(args, "installation_id", tool_name)?)
            .map_err(|error| ExecutionError::Step(format!("{tool_name} installation: {error}")))?;
    let entity = AppName::parse(require_scope_str(args, "entity", tool_name)?)
        .map_err(|error| ExecutionError::Step(format!("{tool_name} entity: {error}")))?;
    let order =
        parse_optional_typed::<Vec<AppQueryOrder>>(args, "order", tool_name)?.unwrap_or_default();
    let relation_expansions =
        parse_optional_typed::<Vec<AppRelationExpansion>>(args, "relation_expansions", tool_name)?
            .unwrap_or_default();
    let cursor = match args.get("cursor") {
        None => None,
        Some(Value::String(cursor)) if !cursor.is_empty() => Some(
            AppReference::parse(cursor.clone())
                .map_err(|error| ExecutionError::Step(format!("{tool_name} cursor: {error}")))?,
        ),
        Some(_) => {
            return Err(ExecutionError::Step(format!(
                "{tool_name} cursor must be a non-empty opaque string"
            )))
        },
    };
    let limit = match args.get("limit") {
        None => 20,
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                ExecutionError::Step(format!("{tool_name} limit must be a positive integer"))
            })?,
    };
    let purpose = AppName::parse(default_purpose)
        .map_err(|error| ExecutionError::Step(format!("{tool_name} purpose: {error}")))?;
    let request = AppQueryRequest {
        pagination: Default::default(),
        protocol_version: AppProtocolVersion::V1,
        source_installation_id: installation_id,
        entity,
        select,
        predicate,
        order,
        cursor,
        limit,
        relation_expansions,
        purpose,
    };
    request
        .validate_app_contract(&AppContractLimits::default())
        .map_err(|error| ExecutionError::Step(format!("{tool_name} request: {error}")))?;
    Ok(request)
}

pub(super) fn parse_select(
    args: &Value,
    tool_name: &str,
    required: bool,
) -> Result<Option<Vec<AppFieldPath>>, ExecutionError> {
    let Some(value) = args.get("select") else {
        return if required {
            Err(ExecutionError::Step(format!(
                "{tool_name} requires select[]"
            )))
        } else {
            Ok(None)
        };
    };
    let values = value.as_array().ok_or_else(|| {
        ExecutionError::Step(format!("{tool_name} select must be an array of strings"))
    })?;
    let fields = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| {
                    ExecutionError::Step(format!("{tool_name} select must contain only strings"))
                })
                .and_then(|field| {
                    AppFieldPath::parse(field).map_err(|error| {
                        ExecutionError::Step(format!("{tool_name} select: {error}"))
                    })
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(fields))
}

pub(super) fn parse_optional_typed<T: DeserializeOwned>(
    args: &Value,
    key: &str,
    tool_name: &str,
) -> Result<Option<T>, ExecutionError> {
    args.get(key)
        .cloned()
        .map(|value| {
            serde_json::from_value(value).map_err(|error| {
                ExecutionError::Step(format!("{tool_name} {key} is invalid: {error}"))
            })
        })
        .transpose()
}

pub(super) fn preflight_args(args: &Value, tool_name: &str) -> Result<(), ExecutionError> {
    let limits = AppContractLimits::default();
    let metrics = inspect_json_bounded(args, limits.max_json_nodes()).ok_or_else(|| {
        ExecutionError::Step(format!(
            "{tool_name} arguments exceed the JSON node ceiling"
        ))
    })?;
    if metrics.max_depth > limits.max_json_depth() {
        return Err(ExecutionError::Step(format!(
            "{tool_name} arguments exceed the JSON depth ceiling"
        )));
    }
    if exact_json_encoded_len(args) > limits.max_document_bytes() {
        return Err(ExecutionError::Step(format!(
            "{tool_name} arguments exceed the byte ceiling"
        )));
    }
    Ok(())
}

pub(super) fn reject_unknown_args(
    args: &Value,
    tool_name: &str,
    model_fields: &[&str],
) -> Result<(), ExecutionError> {
    const RUNTIME_FIELDS: &[&str] = &[
        "__principal",
        "__workspace",
        "__agent_id",
        "__chat_session_id",
        "__execution_id",
        "__task_id",
        "__source_kind",
        "__surface",
        "__feature_mode",
        "__source_agent_id",
        "__chat_turn_id",
        "__max_spawned_tasks",
        "__goal_id",
        "__ui_thread_id",
        "timeout_secs",
        "__timeout_secs_from_pack_default",
    ];
    let object = args
        .as_object()
        .ok_or_else(|| ExecutionError::Step(format!("{tool_name} arguments must be an object")))?;
    if let Some(field) = object.keys().find(|field| {
        !RUNTIME_FIELDS.contains(&field.as_str()) && !model_fields.contains(&field.as_str())
    }) {
        return Err(ExecutionError::Step(format!(
            "{tool_name} contains unsupported argument `{field}`"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_query_parses_canonical_predicate_order_cursor_and_relations() {
        let args = serde_json::json!({
            "installation_id": "install-1",
            "entity": "events",
            "select": ["title", "starts_at"],
            "predicate": {
                "root": 0,
                "nodes": [{"kind": "is_null", "field": "starts_at", "negated": true}]
            },
            "order": [{"field": "starts_at", "direction": "ascending"}],
            "cursor": "cursor:next-page",
            "limit": 40,
            "relation_expansions": [{
                "relation": "calendar",
                "select": ["name"],
                "max_depth": 1,
                "max_rows": 5
            }]
        });
        let select = parse_select(&args, "app_data_query", true)
            .unwrap()
            .unwrap();
        let predicate = parse_optional_typed(&args, "predicate", "app_data_query").unwrap();
        let request = parse_query_request(
            &args,
            "app_data_query",
            select,
            predicate,
            "personal_agent_query",
        )
        .unwrap();
        assert!(request.predicate.is_some());
        assert_eq!(request.order.len(), 1);
        assert!(request.cursor.is_some());
        assert_eq!(request.limit, 40);
        assert_eq!(request.relation_expansions.len(), 1);
    }

    #[test]
    fn rich_query_rejects_an_invalid_predicate_arena() {
        let args = serde_json::json!({
            "installation_id": "install-1",
            "entity": "events",
            "select": ["title"],
            "predicate": {"root": 9, "nodes": [{"kind": "is_null", "field": "title"}]}
        });
        let select = parse_select(&args, "app_data_query", true)
            .unwrap()
            .unwrap();
        let predicate = parse_optional_typed(&args, "predicate", "app_data_query").unwrap();
        assert!(parse_query_request(
            &args,
            "app_data_query",
            select,
            predicate,
            "personal_agent_query"
        )
        .is_err());
    }

    #[test]
    fn rich_query_rejects_forged_authority_fields() {
        for forbidden in ["grant_revision", "purpose", "__grant_revision"] {
            let mut args = serde_json::json!({
                "installation_id": "install-1",
                "entity": "events",
                "select": ["title"]
            });
            args.as_object_mut()
                .unwrap()
                .insert(forbidden.to_owned(), serde_json::json!(99));
            assert!(reject_unknown_args(
                &args,
                "app_data_query",
                &[
                    "installation_id",
                    "entity",
                    "select",
                    "predicate",
                    "order",
                    "cursor",
                    "limit",
                    "relation_expansions"
                ]
            )
            .is_err());
        }
    }

    #[test]
    fn operational_unavailable_outcome_is_stable_and_contains_no_domain_details() {
        let value = serde_json::to_value(AppDataUnavailableOutcome {
            protocol_version: AppProtocolVersion::V1,
            status: "unavailable",
            error_code: APP_DATA_UNAVAILABLE_CODE,
        })
        .unwrap();
        assert_eq!(value["status"], "unavailable");
        assert_eq!(value["error_code"], APP_DATA_UNAVAILABLE_CODE);
        for forbidden in [
            "installation_id",
            "schema_revision",
            "grant_revision",
            "policy",
            "projection_handle",
            "sqlite",
        ] {
            assert!(!value.to_string().contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn unavailable_metadata_guard_is_caller_bound_and_non_egressing() {
        let local = app_data_control_metadata_policy(AppAgentProcessingClass::LocalModel);
        assert_eq!(local.classification_floor, AppDataClassification::Sensitive);
        assert_eq!(local.model_processing, AppModelProcessing::LocalOnly);
        assert_eq!(
            local.personal_agent_access,
            AppPersonalAgentAccess::ApprovedProjection
        );
        assert_eq!(local.memory_promotion, AppMemoryPromotion::Denied);
        assert_eq!(local.external_egress, AppExternalEgress::Denied);
        assert!(local.approved_destinations.is_empty());

        let remote = app_data_control_metadata_policy(AppAgentProcessingClass::RemoteModel);
        assert_eq!(remote.model_processing, AppModelProcessing::RemoteAllowed);
    }
}
