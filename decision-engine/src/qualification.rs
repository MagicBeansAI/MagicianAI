//! Classification eligibility lives beside routing policy in the engine.
use crate::EngineState;
use decision_engine_contract::{request::Answer, telemetry::DecisionCallStatus, DecideResponse};
use magician_decision::config::fingerprint;

impl EngineState {
    pub(crate) fn qualify(
        &self,
        operation: &str,
        projection: &str,
        reference: &str,
        reply: &mut DecideResponse,
    ) {
        let Some(policy) = self.config.operations.get(operation) else {
            return;
        };
        let behavior = self.config.behavior_fingerprint(policy);
        for item in &mut reply.batch.items {
            let Some(answer) = item.response.as_ref() else {
                continue;
            };
            let Some(thresholds) = item.thresholds.as_ref() else {
                continue;
            };
            if answer.pack_id != policy.pack || answer.pack_version != policy.pack_version {
                continue;
            }
            let Some(receipt) = reply.model_calls.iter().rev().find(|call| {
                call.status == DecisionCallStatus::Succeeded
                    && call.item_ids.contains(&item.item_id)
                    && call.model == answer.model.model
                    && call.adapter == answer.model.adapter
            }) else {
                continue;
            };
            let threshold_fingerprint = fingerprint(thresholds);
            for (question, value) in &answer.answers {
                let (output, confidence) = match value {
                    Answer::Noul { noul } => (
                        if *noul >= 0.5 { "true" } else { "false" },
                        noul.max(1.0 - noul),
                    ),
                    Answer::Choice {
                        choice, confidence, ..
                    } => (choice.as_str(), *confidence),
                    Answer::Score { confidence, .. } => ("score", *confidence),
                };
                let Some(threshold) = thresholds.get(question.as_str()) else {
                    continue;
                };
                if !confidence.is_finite() || confidence < *threshold {
                    continue;
                }
                let restricted = policy
                    .restricted_outputs
                    .get(question.as_str())
                    .is_some_and(|outputs| outputs.iter().any(|candidate| candidate == output));
                if policy.gate.enabled && policy.allow_unqualified_gate && !restricted {
                    item.eligible_answers.insert(
                        question.as_str().into(),
                        format!("operator-enabled:{behavior}"),
                    );
                    continue;
                }
                let qualified = policy.qualifications.iter().find(|q| {
                    q.validate().is_ok()
                        && q.question == question.as_str()
                        && q.output == output
                        && q.model == answer.model.model
                        && q.provider == receipt.provider
                        && q.pack_version == answer.pack_version
                        && q.threshold_fingerprint == threshold_fingerprint
                        && q.projection_version == projection
                        && q.reference_version == reference
                        && q.behavior_fingerprint == behavior
                        && match value {
                            Answer::Score { score, .. } => {
                                q.consumer_mapping_version.as_deref() == Some(projection)
                                    && q.score_range
                                        .is_some_and(|[lo, hi]| (lo..=hi).contains(score))
                            },
                            _ => q.score_range.is_none(),
                        }
                });
                if let Some(qualification) = qualified {
                    item.eligible_answers
                        .insert(question.as_str().into(), qualification.evidence_id.clone());
                }
            }
        }
    }
}
