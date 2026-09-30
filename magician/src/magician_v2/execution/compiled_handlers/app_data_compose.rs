//! `app_data_compose` — revalidate one source projection and launch a
//! destination app action through the server-owned composition service.

use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;

use super::app_data::require_direct_personal_agent_context;
use super::app_data_query::{preflight_args, reject_unknown_args};
use super::shared::require_scope_str;
use crate::magician_v2::apps::composition_service::{
    AppComposeAndInvokeRequest, AppCompositionService, AppRecordCompositionRecovery,
};
use crate::magician_v2::apps::models::{
    AppFieldPath, AppInstallationId, AppName, AppRecordId, AppReference,
};
use crate::magician_v2::apps::value_mapping::AppValueMappingOperation;
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::compiled_dispatch::publish_current_compiled_app_result_guard;
use crate::magician_v2::execution::error::ExecutionError;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    preflight_args(&args, "app_data_compose")?;
    reject_unknown_args(
        &args,
        "app_data_compose",
        &[
            "source_projection_handle",
            "source_record_id",
            "destination_installation_id",
            "destination_action_id",
            "mapping",
            "idempotency_key",
        ],
    )?;
    let context =
        require_direct_personal_agent_context(Some(resources.as_ref()), &args, "app_data_compose")?;
    let artifact_service = resources
        .artifact_v2_service
        .as_ref()
        .ok_or_else(|| ExecutionError::Step("app_data_compose failed".into()))?;
    let workflow = artifact_service.app_workflow_service();
    let execution_ref = context.authority.execution_ref().clone();
    let now = Utc::now();
    let handle = AppReference::parse(require_scope_str(
        &args,
        "source_projection_handle",
        "app_data_compose",
    )?)
    .map_err(|error| ExecutionError::Step(format!("app_data_compose handle: {error}")))?;
    let source_record_id = args
        .get("source_record_id")
        .and_then(Value::as_str)
        .map(AppRecordId::parse)
        .transpose()
        .map_err(|error| ExecutionError::Step(format!("app_data_compose record: {error}")))?;
    let destination_installation_id = AppInstallationId::parse(require_scope_str(
        &args,
        "destination_installation_id",
        "app_data_compose",
    )?)
    .map_err(|error| ExecutionError::Step(error.to_string()))?;
    let destination_action_id = AppName::parse(require_scope_str(
        &args,
        "destination_action_id",
        "app_data_compose",
    )?)
    .map_err(|error| ExecutionError::Step(error.to_string()))?;
    let idempotency_key = AppReference::parse(require_scope_str(
        &args,
        "idempotency_key",
        "app_data_compose",
    )?)
    .map_err(|error| ExecutionError::Step(error.to_string()))?;
    let mapping = parse_mapping(&args)?;
    let service = AppCompositionService::new(workflow.clone());
    let recovery = service
        .recover_record_composition(
            &context.authenticated,
            &context.authority,
            &destination_installation_id,
            &destination_action_id,
            &idempotency_key,
            &handle,
            source_record_id.as_ref(),
            &mapping,
            resources.as_ref(),
            &context.publication_fence,
            &context.calling_profile_name,
            now,
        )
        .await
        .map_err(|_| ExecutionError::Step("app_data_compose failed".to_owned()))?;
    let legacy_failure = match recovery {
        AppRecordCompositionRecovery::Recovered(governed) => {
            return publish_governed_composition(governed);
        },
        AppRecordCompositionRecovery::LegacyFreshProjectionRequired(governed) => Some(governed),
        AppRecordCompositionRecovery::Miss => None,
    };
    let (source_query, source_page, _) = match workflow.resolve_personal_agent_projection(
        &context.authenticated,
        &execution_ref,
        &handle,
        now,
    ) {
        Ok(projection) => projection,
        Err(_) => {
            if let Some(governed) = legacy_failure {
                return publish_governed_composition(governed);
            }
            return Err(ExecutionError::Step("app_data_compose failed".to_owned()));
        },
    };
    let governed = service
        .compose_and_invoke(
            &context.authenticated,
            context.authority,
            AppComposeAndInvokeRequest {
                source_projection_handle: handle,
                source_query,
                source_page,
                source_record_id,
                destination_installation_id,
                destination_action_id,
                mapping,
                idempotency_key,
            },
            resources.as_ref(),
            &context.publication_fence,
            &context.calling_profile_name,
            now,
        )
        .await
        .map_err(|_| ExecutionError::Step("app_data_compose failed".to_owned()))?;
    let (launch, policy) = governed.into_parts();
    let value = serde_json::to_value(launch)
        .map_err(|_| ExecutionError::Step("app_data_compose failed".to_owned()))?;
    publish_current_compiled_app_result_guard(policy, &value, None)?;
    Ok(value)
}

fn publish_governed_composition(
    governed: crate::magician_v2::apps::composition_service::AppGovernedCompositionLaunch,
) -> Result<Value, ExecutionError> {
    let (outcome, policy) = governed.into_parts();
    let value = serde_json::to_value(outcome)
        .map_err(|_| ExecutionError::Step("app_data_compose failed".to_owned()))?;
    publish_current_compiled_app_result_guard(policy, &value, None)?;
    Ok(value)
}

pub(super) fn parse_mapping(args: &Value) -> Result<Vec<AppValueMappingOperation>, ExecutionError> {
    let values = args
        .get("mapping")
        .and_then(Value::as_array)
        .ok_or_else(|| ExecutionError::Step("app_data_compose requires mapping[]".into()))?;
    values
        .iter()
        .map(|value| {
            if value.get("kind").is_some() {
                return serde_json::from_value(value.clone()).map_err(|error| {
                    ExecutionError::Step(format!("app_data_compose mapping: {error}"))
                });
            }
            let source = value
                .get("source")
                .and_then(Value::as_str)
                .ok_or_else(|| ExecutionError::Step("mapping.source required".into()))?;
            let target = value
                .get("target")
                .and_then(Value::as_str)
                .ok_or_else(|| ExecutionError::Step("mapping.target required".into()))?;
            Ok(AppValueMappingOperation::Select {
                source: AppFieldPath::parse(source)
                    .map_err(|error| ExecutionError::Step(error.to_string()))?,
                target: AppFieldPath::parse(target)
                    .map_err(|error| ExecutionError::Step(error.to_string()))?,
            })
        })
        .collect()
}
