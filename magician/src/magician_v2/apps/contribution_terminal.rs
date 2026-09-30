//! Deterministic terminal contribution projection.
//!
//! The workflow owner calls this only after the exact mutation receipt and
//! typed action result exist. It cannot mint authority: every input binding is
//! a move-only value reconstructed from the current grant, consumed owner
//! approval, and immutable package lock.

use chrono::{DateTime, Duration, Utc};
use magician_app_contract::contribution::{
    content_digest, AppContributionClassification, AppContributionEvidenceClass,
    AppContributionHandlingLabelsV1, AppContributionModelProcessing,
    AppContributionRetractionPolicy, AppContributionSettlementRefV1, AppContributionSourceHeaderV1,
    AppContributionSourceRefV1, AppContributionUpdatePolicy, AppMemoryCandidateProposalV1,
    AppMemoryInvalidationReasonV1, AppPersonalAgentRetrievalProjectionProposalV1,
    APP_CONTRIBUTION_CONTRACT_VERSION, APP_MEMORY_CANDIDATE_CONTRACT_ID,
    APP_RETRIEVAL_PROJECTION_CONTRACT_ID,
};

use super::{
    contribution::{AppContributionError, AppContributionTaskBinding},
    models::{AppActionResult, AppDataClassification, AppDigest, AppModelProcessing, AppName},
    records::{
        AppContributionDestinationBinding, AppContributionFrequency, AppContributionSource,
        AppMutationReceipt, AppPackageRevision,
    },
    workflows::{AppWorkflowCommitIntent, AppWorkflowTaskBinding, AppWorkflowTerminalEffect},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AppPreparedTerminalContribution {
    Memory {
        proposal: AppMemoryCandidateProposalV1,
        frequency: AppContributionFrequency,
        fallback_invalidation_reason: Option<AppMemoryInvalidationReasonV1>,
    },
    PersonalAgentRetrieval {
        proposal: AppPersonalAgentRetrievalProjectionProposalV1,
        frequency: AppContributionFrequency,
        fallback_invalidation_reason: Option<AppMemoryInvalidationReasonV1>,
    },
    SourceInvalidation {
        destination: AppContributionDestinationBinding,
        installation_id: String,
        scope_binding_ref: String,
        workflow_id: String,
        action_id: String,
        port_id: String,
        entity_name: String,
        record_id: String,
        source_event_revision: u64,
        reason: AppMemoryInvalidationReasonV1,
    },
}

pub(crate) fn build_terminal_contributions(
    task: &AppWorkflowTaskBinding,
    bindings: &[AppContributionTaskBinding],
    package_revision: &AppPackageRevision,
    receipt: &AppMutationReceipt,
    result: &AppActionResult<serde_json::Value>,
    intent: &AppWorkflowCommitIntent,
    publication_now: DateTime<Utc>,
) -> Result<Vec<AppPreparedTerminalContribution>, AppContributionError> {
    if bindings.is_empty() {
        return Ok(Vec::new());
    }
    let summary = intent
        .user_visible_summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty());
    let output = result.output.as_ref().ok_or_else(|| {
        AppContributionError::Authority(
            "terminal contribution requires the canonical typed action output".to_owned(),
        )
    })?;
    if receipt.installation_id != task.installation_id
        || output.installation_id != task.installation_id
        || output.package_revision_ref != task.package_revision_ref
        || package_revision.content_digest != *bindings[0].package_content_digest()
    {
        return Err(AppContributionError::Authority(
            "terminal contribution settlement substituted task/package identity".to_owned(),
        ));
    }

    let issued_at_ms = output.produced_at.timestamp_millis();
    let settlement = AppContributionSettlementRefV1::Mutation {
        mutation_receipt_id: receipt.receipt_id.to_string(),
        first_change_sequence: receipt.change_seq_range.first,
        last_change_sequence: receipt.change_seq_range.last,
    };
    let labels = contribution_labels(&output.handling_labels);
    let mut prepared = Vec::with_capacity(bindings.len());

    for binding in bindings {
        if binding.task_id() != task.task_id
            || binding.installation_id() != &task.installation_id
            || binding.installation_generation() != task.installation_generation
            || binding.package_revision_ref() != &task.package_revision_ref
            || binding.package_lock_digest() != &task.package_lock_digest
            || binding.grant_revision() != task.accepted_authority.grant_revision
            || binding.workflow_id() != &task.workflow_id
            || binding.action_id() != &task.invocation.action_id
        {
            return Err(AppContributionError::Authority(
                "terminal contribution task binding substituted producer identity".to_owned(),
            ));
        }
        let reviewed = binding.reviewed_grant();
        if reviewed.purposes.len() != 1
            || reviewed.evidence_classes.len() != 1
            || reviewed.audiences.len() != 1
            || reviewed.evidence_classes[0] != AppContributionEvidenceClass::Hypothesis
        {
            return Err(AppContributionError::Authority(
                "V1 model-written terminal contribution requires one hypothesis purpose and \
                 audience"
                    .to_owned(),
            ));
        }
        let AppContributionSource::MutationBackedEntityProjection {
            entity,
            selected_fields,
        } = &reviewed.source;
        let deleted_record_id =
            match &intent.effect {
                AppWorkflowTerminalEffect::Mutation { command } => command
                    .operations
                    .iter()
                    .find_map(|operation| match operation {
                        super::models::AppMutationOperation::Delete {
                            entity: operation_entity,
                            record_id,
                        } if operation_entity == entity => Some(record_id),
                        _ => None,
                    }),
                AppWorkflowTerminalEffect::ReadOnly { .. } => None,
            };
        let exact_record_id =
            match &intent.effect {
                AppWorkflowTerminalEffect::Mutation { command } => command
                    .operations
                    .iter()
                    .find_map(|operation| match operation {
                        super::models::AppMutationOperation::Update {
                            entity: operation_entity,
                            record_id,
                            ..
                        }
                        | super::models::AppMutationOperation::Restore {
                            entity: operation_entity,
                            record_id,
                        }
                        | super::models::AppMutationOperation::Delete {
                            entity: operation_entity,
                            record_id,
                        } if operation_entity == entity => Some(record_id),
                        _ => None,
                    }),
                AppWorkflowTerminalEffect::ReadOnly { .. } => None,
            };
        let matching = receipt
            .committed_record_revisions
            .iter()
            .filter(|record| {
                &record.entity == entity
                    && exact_record_id.map_or(true, |record_id| &record.record_id == record_id)
            })
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(AppContributionError::Authority(
                "terminal contribution mutation receipt does not identify one exact source record"
                    .to_owned(),
            ));
        }
        let source_record = matching[0];
        if let Some(deleted_record_id) = deleted_record_id {
            if &source_record.record_id != deleted_record_id {
                return Err(AppContributionError::Authority(
                    "terminal contribution deletion receipt substituted its source record"
                        .to_owned(),
                ));
            }
            prepared.push(source_invalidation(
                task,
                binding,
                entity,
                source_record,
                AppMemoryInvalidationReasonV1::SourceDeleted,
            ));
            continue;
        }
        let fallback_invalidation_reason =
            match &intent.effect {
                AppWorkflowTerminalEffect::Mutation { command } => command
                    .operations
                    .iter()
                    .find_map(|operation| match operation {
                        super::models::AppMutationOperation::Update {
                            entity: operation_entity,
                            record_id,
                            ..
                        } if operation_entity == entity
                            && record_id == &source_record.record_id =>
                        {
                            Some(AppMemoryInvalidationReasonV1::SourceUpdated)
                        },
                        super::models::AppMutationOperation::Restore {
                            entity: operation_entity,
                            record_id,
                        } if operation_entity == entity
                            && record_id == &source_record.record_id =>
                        {
                            Some(AppMemoryInvalidationReasonV1::SourceRestored)
                        },
                        _ => None,
                    }),
                AppWorkflowTerminalEffect::ReadOnly { .. } => None,
            };
        let Some(summary) = summary else {
            if let Some(reason) = fallback_invalidation_reason {
                prepared.push(source_invalidation(
                    task,
                    binding,
                    entity,
                    source_record,
                    reason,
                ));
            }
            continue;
        };
        let selected_fields = selected_fields
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let source_ref_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "schema": "magician.app-contribution-source-ref.v1",
            "installation_id": &task.installation_id,
            "entity": entity,
            "record_id": &source_record.record_id,
            "selected_fields": &selected_fields,
        }))?;
        let source_identity_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "schema": "magician.app-contribution-source-identity.v1",
            "source_ref_digest": &source_ref_digest,
            "record_revision": source_record.revision,
            "handling_labels": &labels,
        }))?;
        let source_ref = format!(
            "entity:{}",
            source_ref_digest.as_str().trim_start_matches("blake3:")
        );
        let source = AppContributionSourceRefV1 {
            installation_id: task.installation_id.to_string(),
            entity_name: entity.to_string(),
            record_id: source_record.record_id.to_string(),
            record_revision: source_record.revision.get(),
            selected_fields,
            canonical_source_ref: source_ref.clone(),
            canonical_source_digest: source_identity_digest.to_string(),
            handling_labels: labels.clone(),
        };
        let dedupe_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "schema": "magician.app-contribution-dedupe.v1",
            "installation_id": &task.installation_id,
            "workflow_id": &task.workflow_id,
            "action_id": &task.invocation.action_id,
            "port_id": binding.port_id(),
            "entity": entity,
            "record_id": &source_record.record_id,
        }))?;
        let proposal_identity = AppDigest::blake3_canonical_json(&serde_json::json!({
            "schema": "magician.app-contribution-proposal-id.v1",
            "binding_digest": binding.binding_digest(),
            "settlement": &settlement,
            "source_identity_digest": &source_identity_digest,
        }))?;
        let expires_at = output
            .produced_at
            .checked_add_signed(Duration::seconds(
                i64::try_from(reviewed.maximum_retention_seconds).map_err(|_| {
                    AppContributionError::Authority(
                        "terminal contribution retention exceeds timestamp range".to_owned(),
                    )
                })?,
            ))
            .ok_or_else(|| {
                AppContributionError::Authority(
                    "terminal contribution expiration overflowed".to_owned(),
                )
            })?;
        // A response-lost entity commit may be recovered after the reviewed
        // candidate TTL. No destination row could have become visible without
        // the still-missing atomic run-state transaction, so terminalize the
        // canonical result without minting an already-expired proposal.
        if expires_at <= publication_now {
            if let Some(reason) = fallback_invalidation_reason {
                prepared.push(source_invalidation(
                    task,
                    binding,
                    entity,
                    source_record,
                    reason,
                ));
            }
            continue;
        }
        let (destination_contract_id, destination_schema_digest) =
            match binding.destination_binding() {
                AppContributionDestinationBinding::MemoryUserKnowledge => (
                    APP_MEMORY_CANDIDATE_CONTRACT_ID,
                    content_digest(b"magician.memory.candidate.schema.v1"),
                ),
                AppContributionDestinationBinding::PersonalAssistantRetrievalNoGoal => (
                    APP_RETRIEVAL_PROJECTION_CONTRACT_ID,
                    content_digest(b"magician.personal-agent.retrieval-projection.schema.v1"),
                ),
            };
        let header = AppContributionSourceHeaderV1 {
            contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            destination_contract_id: destination_contract_id.to_owned(),
            destination_contract_version: APP_CONTRIBUTION_CONTRACT_VERSION,
            destination_schema_digest,
            proposal_id: format!(
                "contribution-proposal:{}",
                proposal_identity.as_str().trim_start_matches("blake3:")
            ),
            proposal_revision: source_record.revision.get(),
            scope_binding_ref: task.accepted_authority.scope_binding_ref.to_string(),
            installation_id: task.installation_id.to_string(),
            installation_generation: task.installation_generation,
            package_revision_ref: task.package_revision_ref.to_string(),
            package_content_digest: package_revision.content_digest.to_string(),
            grant_revision: task.accepted_authority.grant_revision.get(),
            grant_authority_digest: binding.grant_authority_digest().to_string(),
            schema_revision: task.accepted_authority.schema_revision.get(),
            schema_digest: package_revision.entity_schema_digest.to_string(),
            workflow_id: task.workflow_id.to_string(),
            workflow_digest: binding.workflow_declaration_digest().to_string(),
            action_id: task.invocation.action_id.to_string(),
            action_digest: binding.action_declaration_digest().to_string(),
            contribution_port_id: binding.port_id().to_string(),
            contribution_port_digest: binding.locked_port_digest().to_string(),
            settlement: settlement.clone(),
            sources: vec![source],
            handling_labels: labels.clone(),
            purpose: reviewed.purposes[0].to_string(),
            audiences: reviewed.audiences.iter().map(ToString::to_string).collect(),
            evidence_class: reviewed.evidence_classes[0],
            issued_at_ms,
            expires_at_ms: expires_at.timestamp_millis(),
            dedupe_key: format!(
                "contribution:{}",
                dedupe_digest.as_str().trim_start_matches("blake3:")
            ),
            update_policy: AppContributionUpdatePolicy::ReplaceExactSourceHead,
            retraction_policy: AppContributionRetractionPolicy::TombstoneOnAnySourceDrift,
        };
        let frequency = reviewed.frequency;
        match binding.destination_binding() {
            AppContributionDestinationBinding::MemoryUserKnowledge => {
                let (intended_tier_scope, semantic_destination) =
                    binding.destination_binding().memory_body().ok_or_else(|| {
                        AppContributionError::Authority(
                            "memory contribution lost its fixed destination body".to_owned(),
                        )
                    })?;
                let proposal = AppMemoryCandidateProposalV1 {
                    header,
                    intended_tier_scope,
                    semantic_destination,
                    claim_or_summary: summary.to_owned(),
                    claim_digest: content_digest(summary.as_bytes()),
                    evidence_refs: vec![source_ref],
                    proposal_digest: String::new(),
                }
                .seal()?;
                prepared.push(AppPreparedTerminalContribution::Memory {
                    proposal,
                    frequency,
                    fallback_invalidation_reason,
                });
            },
            AppContributionDestinationBinding::PersonalAssistantRetrievalNoGoal => {
                let (target_agent_id, target_goal_id) = binding
                    .destination_binding()
                    .retrieval_target()
                    .ok_or_else(|| {
                        AppContributionError::Authority(
                            "retrieval contribution lost its fixed destination body".to_owned(),
                        )
                    })?;
                let proposal = AppPersonalAgentRetrievalProjectionProposalV1 {
                    header,
                    target_agent_id: target_agent_id.to_owned(),
                    target_goal_id: target_goal_id.map(str::to_owned),
                    projection_text: summary.to_owned(),
                    projection_digest: content_digest(summary.as_bytes()),
                    proposal_digest: String::new(),
                }
                .seal()?;
                prepared.push(AppPreparedTerminalContribution::PersonalAgentRetrieval {
                    proposal,
                    frequency,
                    fallback_invalidation_reason,
                });
            },
        }
    }
    Ok(prepared)
}

fn source_invalidation(
    task: &AppWorkflowTaskBinding,
    binding: &AppContributionTaskBinding,
    entity: &super::models::AppName,
    source_record: &super::records::AppCommittedRecordRevision,
    reason: AppMemoryInvalidationReasonV1,
) -> AppPreparedTerminalContribution {
    AppPreparedTerminalContribution::SourceInvalidation {
        destination: binding.destination_binding(),
        installation_id: task.installation_id.to_string(),
        scope_binding_ref: task.accepted_authority.scope_binding_ref.to_string(),
        workflow_id: task.workflow_id.to_string(),
        action_id: task.invocation.action_id.to_string(),
        port_id: binding.port_id().to_string(),
        entity_name: entity.to_string(),
        record_id: source_record.record_id.to_string(),
        source_event_revision: source_record.revision.get(),
        reason,
    }
}

pub(crate) fn mutation_has_contribution_transition(
    bindings: &[AppContributionTaskBinding],
    operations: &[super::models::AppMutationOperation],
) -> Result<bool, AppContributionError> {
    if bindings.is_empty() {
        return Ok(false);
    }
    let source_entity = bindings[0].reviewed_grant().source_entity();
    if bindings
        .iter()
        .any(|binding| binding.reviewed_grant().source_entity() != source_entity)
    {
        return Err(AppContributionError::Authority(
            "one workflow contribution set selected inconsistent source entities".to_owned(),
        ));
    }
    let mut live_source_mutations = 0usize;
    let mut deletions = 0usize;
    for operation in operations {
        use super::models::AppMutationOperation::{Create, Delete, Restore, Update};
        match operation {
            Create { entity, .. } | Update { entity, .. } | Restore { entity, .. }
                if entity == source_entity =>
            {
                live_source_mutations = live_source_mutations.saturating_add(1);
            },
            Delete { entity, .. } if entity == source_entity => {
                deletions = deletions.saturating_add(1);
            },
            _ => {},
        }
    }
    if deletions != 0 && live_source_mutations != 0 {
        return Err(AppContributionError::Authority(
            "one terminal effect cannot both delete and publish the contribution source".to_owned(),
        ));
    }
    if live_source_mutations > 1 || deletions > 1 {
        return Err(AppContributionError::Authority(
            "V1 contribution transition requires exactly one source-record mutation".to_owned(),
        ));
    }
    Ok(deletions == 1 || live_source_mutations == 1)
}

/// Return the exact maximum number of contribution rows the terminal
/// projection can prepare from the declared mutation and summary, before any
/// entity-store state changes. Receipt validation can still reduce this to an
/// error, but cannot increase it.
pub(crate) fn planned_terminal_contribution_count(
    bindings: &[AppContributionTaskBinding],
    operations: &[super::models::AppMutationOperation],
    user_visible_summary: Option<&str>,
) -> Result<usize, AppContributionError> {
    if !mutation_has_contribution_transition(bindings, operations)? {
        return Ok(0);
    }
    let source_entity = bindings[0].reviewed_grant().source_entity();
    Ok(planned_contribution_count_for_source(
        bindings.len(),
        source_entity,
        operations,
        user_visible_summary,
    ))
}

fn planned_contribution_count_for_source(
    binding_count: usize,
    source_entity: &AppName,
    operations: &[super::models::AppMutationOperation],
    user_visible_summary: Option<&str>,
) -> usize {
    if user_visible_summary.is_some_and(|summary| !summary.trim().is_empty()) {
        return binding_count;
    }
    let emits_invalidation = operations.iter().any(|operation| {
        matches!(
            operation,
            super::models::AppMutationOperation::Update { entity, .. }
                | super::models::AppMutationOperation::Restore { entity, .. }
                | super::models::AppMutationOperation::Delete { entity, .. }
                if entity == source_entity
        )
    });
    if emits_invalidation {
        binding_count
    } else {
        0
    }
}

fn contribution_labels(
    labels: &super::models::AppHandlingLabels,
) -> AppContributionHandlingLabelsV1 {
    AppContributionHandlingLabelsV1 {
        classification: match labels.classification {
            AppDataClassification::Public => AppContributionClassification::Public,
            AppDataClassification::Ordinary => AppContributionClassification::Internal,
            AppDataClassification::Personal => AppContributionClassification::Personal,
            AppDataClassification::Sensitive => AppContributionClassification::Sensitive,
            AppDataClassification::Secret => AppContributionClassification::Secret,
        },
        model_processing: match labels.model_processing {
            AppModelProcessing::None => AppContributionModelProcessing::None,
            AppModelProcessing::LocalOnly => AppContributionModelProcessing::LocalOnly,
            AppModelProcessing::RemoteAllowed => AppContributionModelProcessing::RemoteAllowed,
        },
        policy_digest: labels.policy_digest.to_string(),
        provenance_digest: labels.provenance_digest.to_string(),
    }
}

trait ReviewedContributionSourceEntity {
    fn source_entity(&self) -> &AppName;
}

impl ReviewedContributionSourceEntity for super::records::AppReviewedContributionPortGrant {
    fn source_entity(&self) -> &AppName {
        match &self.source {
            AppContributionSource::MutationBackedEntityProjection { entity, .. } => entity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::models::{AppMutationOperation, AppRecordId};
    use super::*;

    fn source_entity() -> AppName {
        AppName::parse("note").unwrap()
    }

    fn source_record() -> AppRecordId {
        AppRecordId::parse("record-1").unwrap()
    }

    #[test]
    fn planned_count_tracks_exact_rows_for_summary_and_invalidation_paths() {
        let entity = source_entity();
        let create = AppMutationOperation::Create {
            entity: entity.clone(),
            temporary_id: AppName::parse("draft-1").unwrap(),
            record_id: None,
            payload: serde_json::json!({"title": "one"}),
        };
        let update = AppMutationOperation::Update {
            entity: entity.clone(),
            record_id: source_record(),
            patch: serde_json::json!({"title": "two"}),
        };

        assert_eq!(
            planned_contribution_count_for_source(3, &entity, &[create.clone()], Some("summary")),
            3
        );
        assert_eq!(
            planned_contribution_count_for_source(3, &entity, &[create], Some("   ")),
            0
        );
        assert_eq!(
            planned_contribution_count_for_source(3, &entity, &[update], None),
            3
        );
    }
}
