//! `app_action_compose` — reopen one canonical terminal app result, map it,
//! and launch a destination app action without disclosing source bytes to the
//! calling model.

use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;

use super::app_data::require_direct_personal_agent_context;
use super::app_data_query::{preflight_args, reject_unknown_args};
use super::shared::require_scope_str;
use crate::magician_v2::apps::composition_service::{
    AppComposeActionResultRequest, AppCompositionService,
};
use crate::magician_v2::apps::models::{AppInstallationId, AppName, AppReference};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::compiled_dispatch::publish_current_compiled_app_result_guard;
use crate::magician_v2::execution::error::ExecutionError;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    preflight_args(&args, "app_action_compose")?;
    reject_unknown_args(
        &args,
        "app_action_compose",
        &[
            "source_run_ref",
            "destination_installation_id",
            "destination_action_id",
            "mapping",
            "idempotency_key",
        ],
    )?;
    let context = require_direct_personal_agent_context(
        Some(resources.as_ref()),
        &args,
        "app_action_compose",
    )?;
    let artifact_service = resources.artifact_v2_service.as_ref().ok_or_else(|| {
        ExecutionError::Step("app_action_compose app workflow runtime is unavailable".into())
    })?;
    let source_run_ref = AppReference::parse(require_scope_str(
        &args,
        "source_run_ref",
        "app_action_compose",
    )?)
    .map_err(|error| ExecutionError::Step(format!("app_action_compose source run: {error}")))?;
    let destination_installation_id = AppInstallationId::parse(require_scope_str(
        &args,
        "destination_installation_id",
        "app_action_compose",
    )?)
    .map_err(|error| ExecutionError::Step(error.to_string()))?;
    let destination_action_id = AppName::parse(require_scope_str(
        &args,
        "destination_action_id",
        "app_action_compose",
    )?)
    .map_err(|error| ExecutionError::Step(error.to_string()))?;
    let idempotency_key = AppReference::parse(require_scope_str(
        &args,
        "idempotency_key",
        "app_action_compose",
    )?)
    .map_err(|error| ExecutionError::Step(error.to_string()))?;
    let mapping = super::app_data_compose::parse_mapping(&args)?;
    let service = AppCompositionService::new(artifact_service.app_workflow_service());
    let governed = service
        .compose_action_result_and_invoke(
            &context.authenticated,
            context.authority,
            AppComposeActionResultRequest {
                source_run_ref,
                destination_installation_id,
                destination_action_id,
                mapping,
                idempotency_key,
                authority_chain: None,
            },
            resources.as_ref(),
            &context.publication_fence,
            &context.calling_profile_name,
            Utc::now(),
        )
        .await
        .map_err(|_| {
            // Pre-source contract/control failures carry no governed value. Do
            // not surface internal distinctions that could later become a
            // source-existence or policy oracle as this primitive expands.
            ExecutionError::Step("app_action_compose failed".to_owned())
        })?;
    let (result, policy) = governed.into_parts();
    let value = serde_json::to_value(result)
        .map_err(|_| ExecutionError::Step("app_action_compose failed".to_owned()))?;
    publish_current_compiled_app_result_guard(policy, &value, None)?;
    Ok(value)
}
