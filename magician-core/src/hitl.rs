//! Wire contract for opening a HITL prompt without consulting a feed projection.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HitlOpenIdentifiers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_state_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HitlOpenScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Source-specific workflow key used by clarification/plan responders.
    /// This is intentionally distinct from the durable runtime execution id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

/// Complete prompt payload carried by projections that are not the Attention
/// feed itself. `id` is the canonical `/hitl/{id}/respond` key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HitlOpenTarget {
    pub id: String,
    pub source: String,
    pub input_type: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default = "empty_object")]
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub identifiers: HitlOpenIdentifiers,
    #[serde(default)]
    pub scope: HitlOpenScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
}

fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_roundtrips_as_the_frontend_open_contract() {
        let target = HitlOpenTarget {
            id: "question-7".to_string(),
            source: "clarification".to_string(),
            input_type: "choice".to_string(),
            prompt: "Which release channel?".to_string(),
            hint: None,
            input_schema: serde_json::json!({
                "options": [{"id": "stable", "label": "Stable"}]
            }),
            identifiers: HitlOpenIdentifiers {
                correlation_id: Some("question-7".to_string()),
                ..Default::default()
            },
            scope: HitlOpenScope {
                principal: Some("principal-a".to_string()),
                workspace: Some("workspace-a".to_string()),
                workflow_id: Some("task-4".to_string()),
                task_id: Some("task-4".to_string()),
                execution_id: Some("planexec-4".to_string()),
                ..Default::default()
            },
            at: Some(7),
        };

        let value = serde_json::to_value(&target).expect("serialize target");
        assert_eq!(value["id"], "question-7");
        assert_eq!(value["scope"]["principal"], "principal-a");
        assert_eq!(value["scope"]["workspace"], "workspace-a");
        assert_eq!(value["scope"]["workflow_id"], "task-4");
        assert_eq!(value["scope"]["execution_id"], "planexec-4");
        assert_eq!(value["input_schema"]["options"][0]["id"], "stable");
        assert_eq!(
            serde_json::from_value::<HitlOpenTarget>(value).expect("deserialize target"),
            target
        );
    }
}
