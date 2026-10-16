//! Training must request the same ordered vector serving uses.

use super::super::actionability::{
    feature_vector, ActionabilityFeatureInput, ActionabilityFeatureVector,
};

pub fn training_feature_vector(
    input: &ActionabilityFeatureInput,
) -> Option<ActionabilityFeatureVector> {
    feature_vector(input)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::attention::learning::actionability::{
        ChannelAttentionSemanticEnvelope, SemanticExtractorIdentity, ACTIONABILITY_FEATURE_CONTRACT,
    };

    fn semantic_candidate() -> ActionabilityFeatureInput {
        let semantic = ChannelAttentionSemanticEnvelope::from_optional_value(
            Some(&serde_json::json!({
                "communication_type": "direct_request",
                "requested_action": "reply",
                "action_owner": "owner",
                "direct_request_probability": 0.9,
                "broadcast_probability": 0.05,
                "personal_obligation_probability": 0.85,
                "information_value_probability": 0.4,
                "deadline": {"kind": "none", "value": null},
                "campaign_or_event_identity": null,
                "evidence_refs": ["latest_intent", "needs_reply_hint"]
            })),
            7,
            "1.1.0",
            &SemanticExtractorIdentity::default(),
        );
        ActionabilityFeatureInput {
            semantic: Some(semantic),
            classifier_label: Some("needs_reply".to_string()),
            classifier_confidence: Some(0.8),
            latest_direction: Some("inbound".to_string()),
            latest_intent: Some("request".to_string()),
            needs_reply_hint: true,
            message_count: 3,
            age_days: Some(1.5),
            slice1_actionability_probability: Some(0.6),
        }
    }

    #[test]
    fn trainer_and_serving_agree_on_the_feature_vector() {
        let candidate = semantic_candidate();
        let trained = training_feature_vector(&candidate).expect("semantic candidate has features");
        let served = super::super::super::actionability::feature_vector(&candidate)
            .expect("semantic candidate has features");
        assert_eq!(
            trained.names, served.names,
            "feature ORDER is part of the contract"
        );
        assert_eq!(trained.values, served.values);
        assert_eq!(trained.contract, ACTIONABILITY_FEATURE_CONTRACT);
        assert_eq!(served.contract, ACTIONABILITY_FEATURE_CONTRACT);
    }
}
