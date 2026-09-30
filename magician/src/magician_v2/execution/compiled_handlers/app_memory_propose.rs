//! `app_memory_propose` — turn one governed app projection into a reviewed,
//! source-linked memory candidate without auto-promoting the record.

use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use tracing::warn;

use super::app_data::require_direct_personal_agent_context;
use super::app_data_query::{preflight_args, reject_unknown_args};
use super::shared::require_scope_str;
use crate::magician_v2::apps::boundary::AppAgentProcessingClass;
use crate::magician_v2::apps::memory::{AppMemorySemanticDestination, AppMemoryTierScope};
use crate::magician_v2::apps::memory_proposal::{
    AppMemoryProposalRequest, AppMemoryProposalService,
};
use crate::magician_v2::apps::models::{
    AppDataClassification, AppModelProcessing, AppProtocolVersion, AppRecordId, AppReference,
};
use crate::magician_v2::apps::records::{
    AppDataHandlingPolicy, AppExternalEgress, AppMemoryPromotion, AppPersonalAgentAccess,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::compiled_dispatch::publish_current_compiled_app_result_guard;
use crate::magician_v2::execution::error::ExecutionError;

const ALLOWED_MODEL_FIELDS: &[&str] = &[
    "source_projection_handle",
    "source_record_id",
    "candidate_id",
    "intended_tier_scope",
    "semantic_destination",
    "derived_claim_or_summary",
];

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    preflight_args(&args, "app_memory_propose")?;
    reject_unknown_args(&args, "app_memory_propose", ALLOWED_MODEL_FIELDS)?;
    let context = require_direct_personal_agent_context(
        Some(resources.as_ref()),
        &args,
        "app_memory_propose",
    )?;
    let processing_class = context.authority.processing_class();
    let artifact_service = resources.artifact_v2_service.as_ref().ok_or_else(|| {
        ExecutionError::Step("app_memory_propose app registry runtime is unavailable".into())
    })?;
    let workflow = artifact_service.app_workflow_service();
    let execution_ref = context.authority.execution_ref().clone();
    let now = Utc::now();
    let handle = AppReference::parse(require_scope_str(
        &args,
        "source_projection_handle",
        "app_memory_propose",
    )?)
    .map_err(|error| ExecutionError::Step(format!("app_memory_propose handle: {error}")))?;
    let (source_query, source_page, _) = match workflow.resolve_personal_agent_projection(
        &context.authenticated,
        &execution_ref,
        &handle,
        now,
    ) {
        Ok(projection) => projection,
        Err(error) => {
            warn!(error = ?error, "app-memory source projection is unavailable");
            return publish_memory_unavailable(processing_class, false, false);
        },
    };
    let source_record_id = args
        .get("source_record_id")
        .and_then(Value::as_str)
        .map(AppRecordId::parse)
        .transpose()
        .map_err(|error| ExecutionError::Step(format!("app_memory_propose record: {error}")))?;
    let candidate_id = AppReference::parse(require_scope_str(
        &args,
        "candidate_id",
        "app_memory_propose",
    )?)
    .map_err(|error| ExecutionError::Step(format!("app_memory_propose candidate: {error}")))?;
    let intended_tier_scope = serde_json::from_value::<AppMemoryTierScope>(
        args.get("intended_tier_scope").cloned().ok_or_else(|| {
            ExecutionError::Step("app_memory_propose requires intended_tier_scope".to_owned())
        })?,
    )
    .map_err(|error| ExecutionError::Step(format!("app_memory_propose tier: {error}")))?;
    if !memory_tier_targets_current_agent(&intended_tier_scope, &context.target_agent_id) {
        return Err(ExecutionError::Step(
            "app_memory_propose cannot target another agent's memory tier".to_owned(),
        ));
    }
    let semantic_destination = serde_json::from_value::<AppMemorySemanticDestination>(
        args.get("semantic_destination").cloned().ok_or_else(|| {
            ExecutionError::Step("app_memory_propose requires semantic_destination".to_owned())
        })?,
    )
    .map_err(|error| ExecutionError::Step(format!("app_memory_propose destination: {error}")))?;
    let derived_claim_or_summary =
        require_scope_str(&args, "derived_claim_or_summary", "app_memory_propose")?;
    let cancellation = crate::magician_v2::execution::compiled_dispatch::EXECUTION_CANCEL_TOKEN
        .try_with(Clone::clone)
        .unwrap_or(None);
    let service = AppMemoryProposalService::new(workflow.registry_service());
    let governed = match service
        .propose(
            &context.authenticated,
            context.authority,
            AppMemoryProposalRequest {
                source_query,
                source_page,
                source_record_id,
                candidate_id,
                intended_tier_scope,
                semantic_destination,
                derived_claim_or_summary,
            },
            resources.as_ref(),
            &context.publication_fence,
            &context.calling_profile_name,
            cancellation.as_ref(),
            now,
        )
        .await
    {
        Ok(governed) => governed,
        Err(error) => {
            let effect_uncertain = error.effect_may_be_committed();
            warn!(
                error = ?error,
                effect_uncertain,
                "app-memory proposal is unavailable"
            );
            return publish_memory_unavailable(
                processing_class,
                effect_uncertain,
                effect_uncertain,
            );
        },
    };
    let (outcome, policy) = governed.into_parts();
    let value = match serde_json::to_value(outcome) {
        Ok(value) => value,
        Err(error) => {
            warn!(error = ?error, "app-memory proposal result serialization failed");
            return publish_memory_unavailable(processing_class, true, true);
        },
    };
    publish_current_compiled_app_result_guard(policy, &value, None)?;
    Ok(value)
}

#[derive(Serialize)]
struct AppMemoryProposalUnavailable {
    protocol_version: AppProtocolVersion,
    status: &'static str,
    error_code: &'static str,
    retryable: bool,
    effect_uncertain: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    effect_committed: Option<bool>,
}

fn publish_memory_unavailable(
    processing_class: AppAgentProcessingClass,
    retryable: bool,
    effect_uncertain: bool,
) -> Result<Value, ExecutionError> {
    let value = serde_json::to_value(AppMemoryProposalUnavailable {
        protocol_version: AppProtocolVersion::V1,
        status: "unavailable",
        error_code: if effect_uncertain {
            "app_memory_outcome_uncertain"
        } else {
            "app_memory_unavailable"
        },
        retryable,
        effect_uncertain,
        effect_committed: (!effect_uncertain).then_some(false),
    })
    .map_err(|_| ExecutionError::Step("app_memory_propose unavailable".to_owned()))?;
    publish_current_compiled_app_result_guard(
        memory_control_metadata_policy(processing_class),
        &value,
        None,
    )?;
    Ok(value)
}

fn memory_control_metadata_policy(
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

fn memory_tier_targets_current_agent(scope: &AppMemoryTierScope, target_agent_id: &str) -> bool {
    let intended_agent = match scope {
        AppMemoryTierScope::User => return true,
        AppMemoryTierScope::Agent { agent_id } | AppMemoryTierScope::AgentGoal { agent_id, .. } => {
            agent_id.as_str()
        },
    };
    intended_agent == target_agent_id
        || intended_agent
            .strip_prefix("agent:")
            .is_some_and(|agent_id| agent_id == target_agent_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_proposal_agent_scope_is_bound_to_typed_invocation_target() {
        let own = AppMemoryTierScope::Agent {
            agent_id: AppReference::parse("agent:personal-assistant").unwrap(),
        };
        let foreign = AppMemoryTierScope::AgentGoal {
            agent_id: AppReference::parse("agent:researcher").unwrap(),
            goal_id: AppReference::parse("goal:quarterly-plan").unwrap(),
        };

        assert!(memory_tier_targets_current_agent(
            &own,
            "personal-assistant"
        ));
        assert!(!memory_tier_targets_current_agent(
            &foreign,
            "personal-assistant"
        ));
        assert!(memory_tier_targets_current_agent(
            &AppMemoryTierScope::User,
            "personal-assistant"
        ));
    }

    #[test]
    fn uncertain_memory_publication_omits_false_commit_claim_and_identity() {
        let value = serde_json::to_value(AppMemoryProposalUnavailable {
            protocol_version: AppProtocolVersion::V1,
            status: "unavailable",
            error_code: "app_memory_outcome_uncertain",
            retryable: true,
            effect_uncertain: true,
            effect_committed: None,
        })
        .expect("serialize uncertain proposal");

        assert_eq!(
            value.get("effect_uncertain").and_then(Value::as_bool),
            Some(true)
        );
        assert!(value.get("effect_committed").is_none());
        for forbidden in ["candidate_id", "source_projection_handle", "policy_digest"] {
            assert!(value.get(forbidden).is_none(), "{forbidden}");
        }
    }

    #[test]
    fn memory_proposal_rejects_caller_selected_authority_fields() {
        let args = serde_json::json!({
            "source_projection_handle": "projection:one",
            "candidate_id": "candidate:one",
            "intended_tier_scope": {"kind": "user"},
            "semantic_destination": "knowledge",
            "derived_claim_or_summary": "claim",
            "__grant_revision": 7
        });
        assert!(reject_unknown_args(&args, "app_memory_propose", ALLOWED_MODEL_FIELDS,).is_err());
    }
}
