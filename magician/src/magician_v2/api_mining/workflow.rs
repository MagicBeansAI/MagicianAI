//! Workflow-replay Phase 2: compiled, replayable workflow graph.
//!
//! A `WorkflowGraph` is the output of `WorkflowCompiler::compile_from_sequences`.
//! Phase 3 executes the graph via direct HTTP without a browser.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowGraph {
    pub id: String,
    pub origin_key: String,
    pub name: String,
    pub steps: Vec<WorkflowStep>,
    #[serde(default)]
    pub data_flows: Vec<DataFlow>,
    pub auth_requirements: AuthRequirements,
    pub confidence: WorkflowConfidence,
    pub compiled_from_sequence_ids: Vec<String>,
    pub last_compiled_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_replayed_at_ms: Option<i64>,
    #[serde(default)]
    pub replay_stats: ReplayStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub id: String,
    pub step_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    #[serde(default)]
    pub param_sources: HashMap<String, ParamSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_if: Option<SkipCondition>,
    #[serde(default)]
    pub browser_only: bool,
    /// Executable browser fallback for mixed-mode workflow replay. Present when
    /// the source sequence captured the browser primitive action and arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_fallback: Option<BrowserFallbackStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrowserFallbackStep {
    pub action: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParamSource {
    Literal { value: String },
    DataFlow { data_flow_id: String },
    UserInput { input_key: String },
    SessionAuth { auth_scheme: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFlow {
    pub id: String,
    pub source_step: String,
    pub source_path: String,
    pub target_step: String,
    pub target_param: String,
    pub inference_method: InferenceMethod,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceMethod {
    AutoMatch,
    LlmInferred,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkipCondition {
    pub source_step: String,
    pub source_path: String,
    pub operator: SkipOperator,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipOperator {
    Equals,
    NotEquals,
    IsEmpty,
    IsNotEmpty,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthRequirements {
    #[serde(default)]
    pub requires_session: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_origin: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowConfidence {
    pub workflow_level: WorkflowMaturity,
    #[serde(default)]
    pub step_confidences: HashMap<String, f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowMaturity {
    Draft,
    Candidate,
    Validated,
    Trusted,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplayStats {
    #[serde(default)]
    pub successful_replays: u32,
    #[serde(default)]
    pub failed_replays: u32,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn workflow_graph_roundtrips_via_serde() {
        let wf = WorkflowGraph {
            id: "wf_test".to_string(),
            origin_key: "example.com".to_string(),
            name: "Weekly export".to_string(),
            steps: vec![WorkflowStep {
                id: "step_0".to_string(),
                step_index: 0,
                capability_id: Some("cap_login".to_string()),
                param_sources: HashMap::new(),
                skip_if: None,
                browser_only: false,
                browser_fallback: None,
            }],
            data_flows: vec![],
            auth_requirements: AuthRequirements::default(),
            confidence: WorkflowConfidence {
                workflow_level: WorkflowMaturity::Draft,
                step_confidences: HashMap::new(),
            },
            compiled_from_sequence_ids: vec!["seq_1".to_string(), "seq_2".to_string()],
            last_compiled_at_ms: 1_780_000_000_000,
            last_replayed_at_ms: None,
            replay_stats: ReplayStats::default(),
        };
        let json = serde_json::to_string(&wf).unwrap();
        let back: WorkflowGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "wf_test");
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.confidence.workflow_level, WorkflowMaturity::Draft);
    }

    #[test]
    fn param_source_tagged_union_roundtrips() {
        let lit = ParamSource::Literal {
            value: "fixed".to_string(),
        };
        let df = ParamSource::DataFlow {
            data_flow_id: "df_1".to_string(),
        };
        for source in [lit, df] {
            let json = serde_json::to_string(&source).unwrap();
            let back: ParamSource = serde_json::from_str(&json).unwrap();
            match (source, back) {
                (ParamSource::Literal { value: a }, ParamSource::Literal { value: b }) => {
                    assert_eq!(a, b)
                },
                (
                    ParamSource::DataFlow { data_flow_id: a },
                    ParamSource::DataFlow { data_flow_id: b },
                ) => assert_eq!(a, b),
                _ => panic!("param source mismatch"),
            }
        }
    }

    #[test]
    fn skip_condition_supports_each_operator() {
        for op in [
            SkipOperator::Equals,
            SkipOperator::NotEquals,
            SkipOperator::IsEmpty,
            SkipOperator::IsNotEmpty,
        ] {
            let cond = SkipCondition {
                source_step: "step_0".to_string(),
                source_path: "$.status".to_string(),
                operator: op,
                value: serde_json::Value::String("ok".to_string()),
            };
            let json = serde_json::to_string(&cond).unwrap();
            let back: SkipCondition = serde_json::from_str(&json).unwrap();
            assert_eq!(back.operator, op);
        }
    }
}
