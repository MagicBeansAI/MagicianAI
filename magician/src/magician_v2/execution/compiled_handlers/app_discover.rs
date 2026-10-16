//! `app_discover` — bounded deferred discovery for direct personal agents.

use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;
use tracing::warn;

use super::app_data::{reattest_personal_agent_publication, require_direct_personal_agent_context};
use super::app_data_query::{preflight_args, reject_unknown_args};
use crate::magician_v2::apps::app_discovery::{
    AppDiscoveryListQuery, AppPersonalAgentDiscoveryRequest, AppPersonalAgentDiscoveryResult,
    AppPersonalAgentDiscoveryService, APP_DISCOVERY_UNAVAILABLE_CODE, DEFAULT_APP_DISCOVERY_LIMIT,
};
use crate::magician_v2::apps::boundary::AppAgentProcessingClass;
use crate::magician_v2::apps::models::{
    AppDataClassification, AppInstallationId, AppModelProcessing,
};
use crate::magician_v2::apps::records::{
    AppDataHandlingPolicy, AppExternalEgress, AppMemoryPromotion, AppPersonalAgentAccess,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::compiled_dispatch::publish_current_compiled_app_result_guard;
use crate::magician_v2::execution::error::ExecutionError;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    preflight_args(&args, "app_discover")?;
    reject_unknown_args(
        &args,
        "app_discover",
        &["installation_id", "query", "cursor", "limit"],
    )?;
    let request = parse_request(&args)?;
    let context =
        require_direct_personal_agent_context(Some(resources.as_ref()), &args, "app_discover")?;
    let processing_class = context.authority.processing_class();
    let unavailable = || AppPersonalAgentDiscoveryResult::Unavailable {
        error_code: APP_DISCOVERY_UNAVAILABLE_CODE,
    };
    let result = match resources.artifact_v2_service.as_ref() {
        Some(artifact_service) => {
            let discovery = AppPersonalAgentDiscoveryService::from_workflow(
                &artifact_service.app_workflow_service(),
            );
            match discovery
                .discover(
                    &context.authenticated,
                    context.authority,
                    request,
                    Utc::now(),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    warn!(
                        error = ?error,
                        "app discovery failed before producing guarded metadata"
                    );
                    unavailable()
                },
            }
        },
        None => {
            warn!("app discovery registry runtime is unavailable");
            unavailable()
        },
    };
    reattest_personal_agent_publication(
        resources.as_ref(),
        &context.authenticated,
        &context.calling_profile_name,
        &context.publication_fence,
        "app_discover",
        Utc::now(),
    )?;
    let value = match serde_json::to_value(result) {
        Ok(value) => value,
        Err(error) => {
            warn!(
                error = ?error,
                "app discovery result serialization is unavailable"
            );
            serde_json::to_value(unavailable())
                .map_err(|_| ExecutionError::Step("app_discover unavailable".to_owned()))?
        },
    };
    publish_current_compiled_app_result_guard(
        discovery_metadata_policy(processing_class),
        &value,
        None,
    )?;
    Ok(value)
}

fn parse_request(args: &Value) -> Result<AppPersonalAgentDiscoveryRequest, ExecutionError> {
    let installation = optional_typed_string(args, "installation_id")?;
    if let Some(installation) = installation {
        if args.get("query").is_some()
            || args.get("cursor").is_some()
            || args.get("limit").is_some()
        {
            return Err(ExecutionError::Step(
                "app_discover installation_id cannot be combined with list arguments".into(),
            ));
        }
        let installation_id = AppInstallationId::parse(installation)
            .map_err(|error| ExecutionError::Step(format!("app_discover installation: {error}")))?;
        return Ok(AppPersonalAgentDiscoveryRequest::Describe(installation_id));
    }

    let limit = match args.get("limit") {
        None => DEFAULT_APP_DISCOVERY_LIMIT,
        Some(value) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| {
                ExecutionError::Step("app_discover limit must be a positive integer".into())
            })?,
    };
    let query = AppDiscoveryListQuery {
        search: optional_typed_string(args, "query")?,
        cursor: optional_typed_string(args, "cursor")?,
        limit,
    };
    query
        .validate()
        .map_err(|error| ExecutionError::Step(format!("app_discover request: {error}")))?;
    Ok(AppPersonalAgentDiscoveryRequest::List(query))
}

fn optional_typed_string(args: &Value, key: &str) -> Result<Option<String>, ExecutionError> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(ExecutionError::Step(format!(
            "app_discover {key} must be a non-empty string"
        ))),
    }
}

/// Schemas and identifiers are guarded host metadata, never transferable app
/// source data. Keep classification and consequence policy fixed for list,
/// empty and concealed detail results. The processing floor follows the
/// freshly re-attested caller so eligible remote agents can consume metadata
/// without weakening the local or deterministic lanes.
fn discovery_metadata_policy(processing_class: AppAgentProcessingClass) -> AppDataHandlingPolicy {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_modes_are_mutually_exclusive() {
        let error = parse_request(&serde_json::json!({
            "installation_id": "install-1",
            "query": "calendar"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("cannot be combined"));
    }

    #[test]
    fn discovery_list_defaults_are_bounded() {
        let request = parse_request(&serde_json::json!({})).unwrap();
        assert!(matches!(
            request,
            AppPersonalAgentDiscoveryRequest::List(AppDiscoveryListQuery {
                limit: DEFAULT_APP_DISCOVERY_LIMIT,
                ..
            })
        ));
    }

    #[test]
    fn discovery_rejects_forged_authority_fields() {
        let args = serde_json::json!({"grant_revision": 99});
        assert!(reject_unknown_args(
            &args,
            "app_discover",
            &["installation_id", "query", "cursor", "limit"]
        )
        .is_err());
    }

    #[test]
    fn discovery_metadata_guard_is_caller_bound_and_non_egressing() {
        let policy = discovery_metadata_policy(AppAgentProcessingClass::LocalModel);
        assert_eq!(
            policy.classification_floor,
            AppDataClassification::Sensitive
        );
        assert_eq!(policy.model_processing, AppModelProcessing::LocalOnly);
        assert_eq!(
            policy.personal_agent_access,
            AppPersonalAgentAccess::ApprovedProjection
        );
        assert_eq!(policy.memory_promotion, AppMemoryPromotion::Denied);
        assert_eq!(policy.external_egress, AppExternalEgress::Denied);
        assert!(policy.approved_destinations.is_empty());

        assert_eq!(
            discovery_metadata_policy(AppAgentProcessingClass::Deterministic).model_processing,
            AppModelProcessing::None
        );
        assert_eq!(
            discovery_metadata_policy(AppAgentProcessingClass::RemoteModel).model_processing,
            AppModelProcessing::RemoteAllowed
        );
    }
}
