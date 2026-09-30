// Intent Classification for MagicianV2
// Determines whether a user message is a new task, slot answer, status query,
// etc.

use serde::{Deserialize, Serialize};

/// Classification of user message intent
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum QueryIntent {
    /// Brand new task/request from the user
    #[default]
    NewTask,

    /// User is answering a pending slot/parameter question
    AnswerElicitation,

    /// User is checking the status of an active workflow
    StatusQuery,

    /// User wants to cancel the active workflow
    Cancellation,

    /// User is asking for clarification about something
    Clarification,

    /// User wants to continue/resume a paused workflow
    ContinueWorkflow,
}

impl QueryIntent {
    /// Get human-readable description of this intent
    pub fn description(&self) -> &'static str {
        match self {
            Self::NewTask => "Starting a new task",
            Self::AnswerElicitation => "Answering a parameter question",
            Self::StatusQuery => "Checking workflow status",
            Self::Cancellation => "Cancelling current task",
            Self::Clarification => "Asking for clarification",
            Self::ContinueWorkflow => "Resuming paused workflow",
        }
    }

    /// Check if this intent requires active workflow context
    pub fn requires_workflow(&self) -> bool {
        matches!(
            self,
            Self::StatusQuery | Self::Cancellation | Self::ContinueWorkflow
        )
    }

    /// Check if this intent expects pending slots
    pub fn expects_slots(&self) -> bool {
        matches!(self, Self::AnswerElicitation)
    }
}

impl std::fmt::Display for QueryIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description())
    }
}

/// Result of matching a user message to a pending slot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotMatchAnalysis {
    /// Which slot this message is answering
    pub slot_id: String,

    /// Extracted value from the message (typed JSON value)
    pub extracted_value: serde_json::Value,

    /// Confidence that this matches the slot (0.0-1.0)
    pub match_confidence: f64,

    /// Why the LLM thinks this matches this specific slot
    pub reasoning: String,

    /// Alternative interpretations if confidence is low
    pub alternatives: Vec<AlternativeSlotMatch>,
}

/// Alternative slot match interpretation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlternativeSlotMatch {
    /// Alternative slot that might match
    pub slot_id: String,

    /// Confidence for this alternative
    pub confidence: f64,

    /// Reasoning for this alternative
    pub reasoning: String,
}

impl SlotMatchAnalysis {
    /// Check if confidence is high enough to proceed without clarification
    pub fn is_confident(&self) -> bool {
        self.match_confidence >= 0.8
    }

    /// Check if clarification should be requested
    pub fn needs_clarification(&self) -> bool {
        self.match_confidence < 0.8 && self.match_confidence > 0.3
    }

    /// Check if the match is too ambiguous to use
    pub fn is_ambiguous(&self) -> bool {
        self.match_confidence <= 0.3 || !self.alternatives.is_empty()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_intent_description() {
        assert_eq!(QueryIntent::NewTask.description(), "Starting a new task");
        assert_eq!(
            QueryIntent::AnswerElicitation.description(),
            "Answering a parameter question"
        );
    }

    #[test]
    fn test_intent_requires_workflow() {
        assert!(!QueryIntent::NewTask.requires_workflow());
        assert!(QueryIntent::StatusQuery.requires_workflow());
        assert!(QueryIntent::Cancellation.requires_workflow());
        assert!(QueryIntent::ContinueWorkflow.requires_workflow());
    }

    #[test]
    fn test_intent_expects_slots() {
        assert!(!QueryIntent::NewTask.expects_slots());
        assert!(QueryIntent::AnswerElicitation.expects_slots());
    }

    #[test]
    fn test_slot_match_confidence() {
        let high_confidence = SlotMatchAnalysis {
            slot_id: "test".to_string(),
            extracted_value: serde_json::json!("value"),
            match_confidence: 0.9,
            reasoning: "Clear match".to_string(),
            alternatives: vec![],
        };
        assert!(high_confidence.is_confident());
        assert!(!high_confidence.needs_clarification());
        assert!(!high_confidence.is_ambiguous());

        let medium_confidence = SlotMatchAnalysis {
            slot_id: "test".to_string(),
            extracted_value: serde_json::json!("value"),
            match_confidence: 0.6,
            reasoning: "Possible match".to_string(),
            alternatives: vec![],
        };
        assert!(!medium_confidence.is_confident());
        assert!(medium_confidence.needs_clarification());
        assert!(!medium_confidence.is_ambiguous());

        let low_confidence = SlotMatchAnalysis {
            slot_id: "test".to_string(),
            extracted_value: serde_json::json!("value"),
            match_confidence: 0.2,
            reasoning: "Unclear".to_string(),
            alternatives: vec![],
        };
        assert!(!low_confidence.is_confident());
        assert!(!low_confidence.needs_clarification());
        assert!(low_confidence.is_ambiguous());
    }
}
