//! `app_action_invoke` — launch one discovered user action through the
//! canonical app-workflow owner.
//!
//! This adapter accepts only destination identity, a stable idempotency key,
//! and typed input. Package/schema/grant/action revisions, provenance, caller
//! identity and result policy are resolved by [`AppWorkflowService`].

use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use serde_json::Value;

use super::app_data::{reattest_personal_agent_publication, require_direct_personal_agent_context};
use super::app_data_query::{preflight_args, reject_unknown_args};
use super::shared::require_scope_str;
use crate::magician_v2::apps::boundary::AppAgentProcessingClass;
use crate::magician_v2::apps::models::{
    AppContractLimits, AppDataClassification, AppDirectActionRequest, AppInstallationId,
    AppModelActionResult, AppModelProcessing, AppName, AppReference, AppRunHandle,
    ValidateAppContract,
};
use crate::magician_v2::apps::records::{
    AppDataHandlingPolicy, AppExternalEgress, AppMemoryPromotion, AppPersonalAgentAccess,
};
use crate::magician_v2::apps::workflows::{
    AppPersonalAgentWorkflowLaunch, AppPersonalAgentWorkflowLaunchError, AppWorkflowError,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::compiled_dispatch::publish_current_compiled_app_result_guard;
use crate::magician_v2::execution::error::ExecutionError;

const ALLOWED_MODEL_FIELDS: &[&str] = &["installation_id", "action_id", "idempotency_key", "input"];

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum AppActionInvokeOutcome {
    Launched {
        run_handle: AppRunHandle,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<AppModelActionResult<Value>>,
        result_withheld_by_policy: bool,
        retryable: bool,
        effect_uncertain: bool,
    },
    Cancelled {
        retryable: bool,
        effect_uncertain: bool,
    },
    Unavailable {
        #[serde(skip_serializing_if = "Option::is_none")]
        run_handle: Option<AppRunHandle>,
        retryable: bool,
        effect_uncertain: bool,
        error_code: &'static str,
    },
}

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    preflight_args(&args, "app_action_invoke")?;
    reject_unknown_model_fields(&args)?;
    let context = require_direct_personal_agent_context(
        Some(resources.as_ref()),
        &args,
        "app_action_invoke",
    )?;
    let processing_class = context.authority.processing_class();
    let installation_id = AppInstallationId::parse(require_scope_str(
        &args,
        "installation_id",
        "app_action_invoke",
    )?)
    .map_err(|_| unavailable_input())?;
    let action_id = AppName::parse(require_scope_str(&args, "action_id", "app_action_invoke")?)
        .map_err(|_| unavailable_input())?;
    let request = AppDirectActionRequest {
        idempotency_key: AppReference::parse(require_scope_str(
            &args,
            "idempotency_key",
            "app_action_invoke",
        )?)
        .map_err(|_| unavailable_input())?,
        input: args.get("input").cloned().ok_or_else(unavailable_input)?,
        expected_installation_binding: None,
    };
    request
        .validate_app_contract(&AppContractLimits::default())
        .map_err(|_| unavailable_input())?;
    let artifact_service = resources
        .artifact_v2_service
        .as_ref()
        .ok_or_else(|| ExecutionError::Step("app_action_invoke unavailable".to_owned()))?;
    let workflow = artifact_service.app_workflow_service();
    let launch_result = workflow
        .invoke_personal_agent_input(
            &context.authenticated,
            context.authority,
            &installation_id,
            &action_id,
            request.idempotency_key,
            request.input,
            resources.as_ref(),
            &context.publication_fence,
            &context.calling_profile_name,
            Utc::now(),
        )
        .await;
    // No success or error DTO carrying app control identity may enter the
    // current model transcript without one last comparison to the original
    // physical-provider/session fence. The workflow also fences its app-data
    // and effect boundaries; this handler fence covers operational error exits
    // which occur after those boundaries but before serialization.
    let launch_result = if reattest_personal_agent_publication(
        resources.as_ref(),
        &context.authenticated,
        &context.calling_profile_name,
        &context.publication_fence,
        "app_action_invoke",
        Utc::now(),
    )
    .is_err()
    {
        Err(AppPersonalAgentWorkflowLaunchError::PublicationDenied {
            effect_uncertain: direct_launch_effect_may_be_uncertain(&launch_result),
        })
    } else {
        launch_result
    };
    let (outcome, guard_policy) = match launch_result {
        Ok(launch) => {
            let (run_handle, result, result_withheld_by_policy, guard_policy) = launch.into_parts();
            (
                AppActionInvokeOutcome::Launched {
                    run_handle,
                    result,
                    result_withheld_by_policy,
                    retryable: false,
                    effect_uncertain: false,
                },
                Some(guard_policy),
            )
        },
        Err(AppPersonalAgentWorkflowLaunchError::CancelledBeforeDispatch) => {
            (
                AppActionInvokeOutcome::Cancelled {
                    // The workflow owner observed cancellation at its final
                    // pre-dispatch fence. No provider/effect boundary was
                    // entered, so an exact same-key retry is both safe and
                    // the canonical way to resume the requested action.
                    retryable: true,
                    effect_uncertain: false,
                },
                None,
            )
        },
        Err(AppPersonalAgentWorkflowLaunchError::PublicationDenied { effect_uncertain }) => {
            (publication_denied_outcome(effect_uncertain), None)
        },
        Err(AppPersonalAgentWorkflowLaunchError::PreDispatch(error)) => (
            AppActionInvokeOutcome::Unavailable {
                run_handle: None,
                retryable: error.is_direct_launch_retryable(),
                effect_uncertain: false,
                error_code: "app_action_unavailable",
            },
            None,
        ),
        Err(AppPersonalAgentWorkflowLaunchError::ExistingRunStateUnknown { error }) => {
            (existing_run_state_unknown_outcome(&error), None)
        },
        Err(AppPersonalAgentWorkflowLaunchError::ExistingNotStarted {
            error,
            run_handle,
            guard_policy,
        }) => (
            AppActionInvokeOutcome::Unavailable {
                run_handle: Some(run_handle),
                retryable: error.is_direct_launch_retryable(),
                effect_uncertain: false,
                error_code: "app_action_unavailable",
            },
            Some(guard_policy),
        ),
        Err(AppPersonalAgentWorkflowLaunchError::EffectUncertain {
            error,
            run_handle,
            guard_policy,
        }) => (
            AppActionInvokeOutcome::Unavailable {
                run_handle: Some(run_handle),
                retryable: error.is_direct_launch_retryable(),
                effect_uncertain: true,
                error_code: "app_action_outcome_uncertain",
            },
            Some(guard_policy),
        ),
    };
    let value = serde_json::to_value(outcome)
        .map_err(|_| ExecutionError::Step("app_action_invoke unavailable".to_owned()))?;
    publish_current_compiled_app_result_guard(
        guard_policy.unwrap_or_else(|| control_metadata_policy(processing_class)),
        &value,
        None,
    )?;
    Ok(value)
}

fn direct_launch_effect_may_be_uncertain(
    result: &Result<AppPersonalAgentWorkflowLaunch, AppPersonalAgentWorkflowLaunchError>,
) -> bool {
    matches!(
        result,
        Ok(_)
            | Err(AppPersonalAgentWorkflowLaunchError::PublicationDenied {
                effect_uncertain: true,
            })
            | Err(AppPersonalAgentWorkflowLaunchError::ExistingRunStateUnknown { .. })
            | Err(AppPersonalAgentWorkflowLaunchError::EffectUncertain { .. })
    )
}

fn reject_unknown_model_fields(args: &Value) -> Result<(), ExecutionError> {
    reject_unknown_args(args, "app_action_invoke", ALLOWED_MODEL_FIELDS)
        .map_err(|_| unavailable_input())
}

fn publication_denied_outcome(effect_uncertain: bool) -> AppActionInvokeOutcome {
    // Keep the public three-state contract and collapse the stale-provider
    // reason. The only truthful recovery fact retained is whether an effect
    // may already have crossed; app/run/result identity stays server-side.
    AppActionInvokeOutcome::Unavailable {
        run_handle: None,
        retryable: true,
        effect_uncertain,
        error_code: if effect_uncertain {
            "app_action_outcome_uncertain"
        } else {
            "app_action_unavailable"
        },
    }
}

fn existing_run_state_unknown_outcome(error: &AppWorkflowError) -> AppActionInvokeOutcome {
    AppActionInvokeOutcome::Unavailable {
        run_handle: None,
        retryable: error.is_direct_launch_retryable(),
        effect_uncertain: true,
        error_code: "app_action_outcome_uncertain",
    }
}

fn control_metadata_policy(processing_class: AppAgentProcessingClass) -> AppDataHandlingPolicy {
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

fn unavailable_input() -> ExecutionError {
    ExecutionError::Step("app_action_invoke input is unavailable".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_caller_selected_authority_and_schema_fields() {
        for forbidden in [
            "schema_revision",
            "grant_revision",
            "package_revision_ref",
            "requested_result_schema_ref",
            "caller_surface_or_execution_ref",
            "provider_profile",
            "__grant_revision",
            "__provider_profile",
        ] {
            let mut args = serde_json::json!({
                "installation_id": "install_1",
                "action_id": "run",
                "idempotency_key": "request:1",
                "input": {}
            });
            args.as_object_mut()
                .expect("object")
                .insert(forbidden.to_owned(), Value::String("forged".to_owned()));
            assert!(reject_unknown_model_fields(&args).is_err(), "{forbidden}");
        }
    }

    #[test]
    fn model_outcomes_never_serialize_runtime_or_authority_internals() {
        let outcome = AppActionInvokeOutcome::Unavailable {
            run_handle: None,
            retryable: false,
            effect_uncertain: false,
            error_code: "app_action_unavailable",
        };
        let encoded = serde_json::to_value(outcome).expect("serialize outcome");
        for forbidden in [
            "task_id",
            "execution_id",
            "schema_revision",
            "grant_revision",
            "package_revision_ref",
            "receipt",
            "policy_digest",
            "provenance",
        ] {
            assert!(!encoded.to_string().contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn stale_publication_stage_never_serializes_recovery_identity_or_result() {
        let encoded = serde_json::to_value(publication_denied_outcome(true))
            .expect("serialize publication denial");

        assert_eq!(
            encoded.get("status").and_then(Value::as_str),
            Some("unavailable")
        );
        assert_eq!(
            encoded.get("error_code").and_then(Value::as_str),
            Some("app_action_outcome_uncertain")
        );
        assert_eq!(
            encoded.get("retryable").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            encoded.get("effect_uncertain").and_then(Value::as_bool),
            Some(true)
        );
        for forbidden in ["run_handle", "result", "installation_id", "action_id"] {
            assert!(encoded.get(forbidden).is_none(), "{forbidden}");
        }
    }

    #[test]
    fn final_publication_denial_preserves_only_effect_uncertainty() {
        assert!(direct_launch_effect_may_be_uncertain(&Err(
            AppPersonalAgentWorkflowLaunchError::PublicationDenied {
                effect_uncertain: true,
            },
        )));
        assert!(!direct_launch_effect_may_be_uncertain(&Err(
            AppPersonalAgentWorkflowLaunchError::PublicationDenied {
                effect_uncertain: false,
            },
        )));
        assert!(!direct_launch_effect_may_be_uncertain(&Err(
            AppPersonalAgentWorkflowLaunchError::CancelledBeforeDispatch,
        )));
    }

    #[test]
    fn unreadable_existing_run_reports_uncertainty_without_identity() {
        let encoded = serde_json::to_value(existing_run_state_unknown_outcome(
            &AppWorkflowError::Registry(
                crate::magician_v2::apps::registry::AppRegistryError::Overloaded,
            ),
        ))
        .expect("serialize unknown existing run");

        assert_eq!(
            encoded.get("status").and_then(Value::as_str),
            Some("unavailable")
        );
        assert_eq!(
            encoded.get("retryable").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            encoded.get("effect_uncertain").and_then(Value::as_bool),
            Some(true)
        );
        for forbidden in ["run_handle", "result", "installation_id", "action_id"] {
            assert!(encoded.get(forbidden).is_none(), "{forbidden}");
        }
    }
}
