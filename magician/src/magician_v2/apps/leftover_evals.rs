//! Deterministic leftover evals for Phase 5C.
//!
//! These do not call a live model. They prove the learning-plan query
//! envelope and a two-app compose path keep source policy, refuse
//! incompatible fields, and never grant destination store access.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    authority::AuthenticatedAppScope,
    boundary::{
        AppAgentProcessingClass, AppDirectOwnerExecutionEvidence, AppPersonalAgentProviderGrant,
        AppPersonalAgentReadAuthority, AppStoreReadAudience,
    },
    composition::{broker_source_to_destination, AppBrokeredTransfer, AppCompositionDestination},
    models::{
        AppDataClassification, AppDataEnvelope, AppDataSource, AppDigest, AppFieldPath,
        AppHandlingLabels, AppInstallationId, AppModelProcessing, AppName, AppProtocolVersion,
        AppReference, AppRevision, AppSourceRef, AppSourceRefKind,
    },
    query_semantics::AppQueryScalarKind,
    records::{
        AppDataHandlingPolicy, AppExternalEgress, AppMemoryPromotion, AppPersonalAgentAccess,
    },
    registry::tests::{authenticated_scope, time},
    value_mapping::{AppValueFieldContract, AppValueMappingOperation, AppValueSchemaContract},
};
use crate::magician_v2::agents::{
    AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPlatformLeftoverEvalReport {
    pub learning_plan_query: bool,
    pub two_app_compose: bool,
}

impl AppPlatformLeftoverEvalReport {
    pub fn passed(&self) -> bool {
        self.learning_plan_query && self.two_app_compose
    }
}

pub fn run_phase5c_leftover_evals() -> AppPlatformLeftoverEvalReport {
    AppPlatformLeftoverEvalReport {
        learning_plan_query: learning_plan_query_eval().is_ok(),
        two_app_compose: two_app_compose_eval().is_ok(),
    }
}

fn learning_plan_query_eval() -> Result<(), String> {
    let auth = authenticated_scope("anonymous", "default");
    let source = envelope(
        &auth,
        "install_plan",
        serde_json::json!({"topic": "graphs", "status": "ready"}),
        labels(
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
        ),
    );
    let dest = policy(
        AppPersonalAgentAccess::ApprovedProjection,
        AppDataClassification::Personal,
        AppModelProcessing::LocalOnly,
        AppExternalEgress::Denied,
    );
    let transfer = broker(
        &auth,
        &source,
        &[("topic", true), ("status", true)],
        "install_today",
        "suggest_next",
        &[("prompt", true), ("state", false)],
        &dest,
        vec![select("topic", "prompt"), select("status", "state")],
        "transfer:learning-plan",
    )
    .map_err(|error| error.to_string())?;
    if transfer.envelope.value["prompt"] != "graphs" {
        return Err("learning-plan topic did not map".to_owned());
    }
    if transfer.admission.receipt().accepted_fields.len() != 2 {
        return Err("learning-plan fields were dropped".to_owned());
    }
    if transfer.envelope.source != AppDataSource::BrokeredTransfer {
        return Err("learning-plan transfer is not brokered".to_owned());
    }
    Ok(())
}

fn two_app_compose_eval() -> Result<(), String> {
    let auth = authenticated_scope("anonymous", "default");
    let source = envelope(
        &auth,
        "install_a",
        serde_json::json!({"name": "Asha", "secret_note": "local-only"}),
        labels(
            AppDataClassification::Personal,
            AppModelProcessing::LocalOnly,
        ),
    );
    let dest = policy(
        AppPersonalAgentAccess::ApprovedProjection,
        AppDataClassification::Personal,
        AppModelProcessing::RemoteAllowed,
        AppExternalEgress::Denied,
    );
    let transfer = broker(
        &auth,
        &source,
        &[("name", true), ("secret_note", false)],
        "install_b",
        "capture_insight",
        &[("title", false), ("summary", false)],
        &dest,
        vec![select("name", "title"), select("secret_note", "summary")],
        "transfer:two-app",
    )
    .map_err(|error| error.to_string())?;
    if transfer.admission.effective_policy().model_processing != AppModelProcessing::LocalOnly {
        return Err("two-app compose broadened local-only source processing".to_owned());
    }
    Ok(())
}

fn broker(
    auth: &AuthenticatedAppScope,
    source: &AppDataEnvelope<Value>,
    source_fields: &[(&str, bool)],
    destination_install: &str,
    action: &str,
    dest_fields: &[(&str, bool)],
    dest: &AppDataHandlingPolicy,
    operations: Vec<AppValueMappingOperation>,
    transfer_id: &str,
) -> Result<AppBrokeredTransfer, super::composition::AppCompositionError> {
    let destination = AppCompositionDestination {
        installation_id: AppInstallationId::parse(destination_install).unwrap(),
        package_revision_ref: AppReference::parse(format!("package:{destination_install}"))
            .unwrap(),
        schema_revision: AppRevision::new(1).unwrap(),
        grant_revision: AppRevision::new(1).unwrap(),
        action_id: AppName::parse(action).unwrap(),
        action_revision: AppRevision::new(1).unwrap(),
        input_schema_ref: AppReference::parse(format!("{action}.input")).unwrap(),
        input_schema: schema(dest_fields),
        result_schema_ref: AppReference::parse(format!("{action}.result")).unwrap(),
        policy: dest.clone(),
    };
    broker_source_to_destination(
        auth,
        &authority(auth),
        source,
        &policy(
            AppPersonalAgentAccess::ApprovedProjection,
            source.handling_labels.classification,
            source.handling_labels.model_processing,
            AppExternalEgress::Denied,
        ),
        &schema(source_fields),
        1,
        &destination,
        operations,
        AppReference::parse(transfer_id).unwrap(),
        time(5),
    )
}

fn labels(
    classification: AppDataClassification,
    model_processing: AppModelProcessing,
) -> AppHandlingLabels {
    let source_policy = policy(
        AppPersonalAgentAccess::ApprovedProjection,
        classification,
        model_processing,
        AppExternalEgress::Denied,
    );
    AppHandlingLabels {
        classification,
        model_processing,
        policy_digest: AppDigest::blake3_canonical_json(
            &serde_json::to_value(source_policy).unwrap(),
        )
        .unwrap(),
        provenance_digest: AppDigest::blake3(b"source-provenance"),
    }
}

fn policy(
    personal_agent: AppPersonalAgentAccess,
    classification: AppDataClassification,
    model_processing: AppModelProcessing,
    egress: AppExternalEgress,
) -> AppDataHandlingPolicy {
    AppDataHandlingPolicy {
        classification_floor: classification,
        model_processing,
        personal_agent_access: personal_agent,
        memory_promotion: AppMemoryPromotion::Denied,
        external_egress: egress,
        approved_destinations: Vec::new(),
    }
}

fn schema(fields: &[(&str, bool)]) -> AppValueSchemaContract {
    let mut map = BTreeMap::new();
    for (name, required) in fields {
        map.insert(
            AppFieldPath::parse(*name).unwrap(),
            AppValueFieldContract {
                kind: AppQueryScalarKind::Text,
                required: *required,
                nullable: false,
                enum_values: Default::default(),
            },
        );
    }
    AppValueSchemaContract::from_compiled_fields(map).unwrap()
}

fn envelope(
    auth: &AuthenticatedAppScope,
    installation: &str,
    value: Value,
    handling: AppHandlingLabels,
) -> AppDataEnvelope<Value> {
    let content_digest = AppDigest::blake3_canonical_json(&value).unwrap();
    AppDataEnvelope {
        protocol_version: AppProtocolVersion::V1,
        source: AppDataSource::AppStore,
        scope_binding_ref: auth.scope_binding_ref().clone(),
        installation_id: AppInstallationId::parse(installation).unwrap(),
        package_revision_ref: AppReference::parse("package:eval").unwrap(),
        schema_revision: AppRevision::new(1).unwrap(),
        grant_revision: AppRevision::new(1).unwrap(),
        value_schema_ref: AppReference::parse("schema:source").unwrap(),
        value,
        source_refs: vec![AppSourceRef {
            kind: AppSourceRefKind::EntityField,
            reference: AppReference::parse("entity:node/record:1").unwrap(),
            revision: Some(AppRevision::new(1).unwrap()),
            fields: vec![AppFieldPath::parse("topic").unwrap()],
        }],
        handling_labels: handling,
        content_digest,
        produced_at: time(4),
        expires_at: None,
    }
}

fn authority(authenticated: &AuthenticatedAppScope) -> AppStoreReadAudience {
    let grant = AppPersonalAgentProviderGrant::from_trusted_provider_registry(
        AppAgentProcessingClass::Deterministic,
        AppDataClassification::Secret,
        AppRevision::new(1).unwrap(),
        AppDigest::blake3(b"provider"),
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
        AppReference::parse("execution:eval").unwrap(),
        grant,
        time(4),
    )
    .unwrap();
    let authority =
        AppPersonalAgentReadAuthority::from_current_execution(authenticated, evidence, time(4))
            .unwrap();
    AppStoreReadAudience::PersonalAgent {
        execution_ref: authority.execution_ref().clone(),
        processing_class: authority.processing_class(),
        maximum_classification: authority.maximum_classification(),
    }
}

fn select(source: &str, target: &str) -> AppValueMappingOperation {
    AppValueMappingOperation::Select {
        source: AppFieldPath::parse(source).unwrap(),
        target: AppFieldPath::parse(target).unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftover_evals_pass_without_a_live_model() {
        let report = run_phase5c_leftover_evals();
        assert!(report.passed(), "{report:?}");
    }
}
