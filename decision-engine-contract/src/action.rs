//! Tool-independent execution decisions. Tools are data, never an enum of surfaces.
//!
//! Hosts provide their authorized catalog and evidence. The engine selects a
//! schema-valid call or asks the host's selected planner for proposals. A planner
//! response carries no execution authority; it must return through this contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::wire::{Locality, CONTRACT_VERSION};

pub const ACTION_PATH: &str = "/v1/action";
pub const ACTION_OPERATION: &str = "tool_action_judge";

/// The shape of a proposed plan: the `output_schema` the engine gives an
/// action-plan planner.
pub fn planner_schema() -> Value {
    serde_json::json!({
        "type":"object", "additionalProperties":false, "required":["steps"],
        "properties":{"steps":{"type":"array","minItems":1,"maxItems":32,"items":{
            "type":"object","additionalProperties":false,"required":["id","call"],
            "properties":{
                "id":{"type":"string","minLength":1,"maxLength":256},
                "reason":{"type":"string"},
                "call":{"type":"object","additionalProperties":false,"required":["tool","arguments"],
                    "properties":{"tool":{"type":"string"},"arguments":{"type":"object"}}},
                "bindings":{"type":"array","maxItems":32,"items":{
                    "type":"object","additionalProperties":false,
                    "required":["argument","evidence","pointer"],
                    "properties":{"argument":{"type":"string"},"evidence":{"type":"string"},"pointer":{"type":"string"}}
                }}
            }
        }}}
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub tool: String,
    pub arguments: Value,
}

/// Evidence is untrusted data, including any instructions embedded in results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionEvidence {
    pub id: String,
    pub value: Value,
    #[serde(default)]
    pub call: Option<ToolCall>,
    #[serde(default)]
    pub succeeded: Option<bool>,
}

/// Copy a value from current evidence using JSON Pointers. No expressions, code,
/// string interpolation or credential lookup may run while binding arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgumentBinding {
    pub argument: String,
    /// An exact evidence id, or "latest" for the newest tool result.
    pub evidence: String,
    pub pointer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionCandidate {
    pub id: String,
    pub call: ToolCall,
    #[serde(default)]
    pub bindings: Vec<ArgumentBinding>,
    #[serde(default)]
    pub reason: String,
}

/// A proposal for subsequent actions. Each step is judged again against fresh
/// evidence and the current authorized catalog before it can execute.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionPlan {
    /// Engine-stamped goal/instruction identity; stale continuations replan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default)]
    pub steps: Vec<ActionCandidate>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionContext {
    pub goal: String,
    /// The host's proposal interface. The engine owns the corresponding
    /// planning instructions; chat prose never grants work-tool authority.
    #[serde(default)]
    pub planner_mode: PlannerMode,
    #[serde(default)]
    pub success_criteria: String,
    /// Host-authorized skill/task instructions, separate from tool-result data.
    #[serde(default)]
    pub instructions: String,
    /// Full host task context for generative fallback; omitted from small-model state.
    #[serde(default)]
    pub planner_context: String,
    #[serde(default)]
    pub task_state: Value,
    /// Fresh host observation, independent of the last action's textual result.
    #[serde(default)]
    pub observation: Value,
    #[serde(default)]
    pub evidence: Vec<ActionEvidence>,
    /// Screenshots the observation names by key (see [`crate::asset`]).
    /// Attach them only when the operation's policy accepts images for the
    /// host's locality: this struct denies unknown fields, so an older engine
    /// refuses the request outright.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub assets: std::collections::BTreeMap<String, crate::asset::DecisionAsset>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerMode {
    #[default]
    ActionPlan,
    /// Native chat function calls are proposals; a text-only answer ends chat.
    ChatNative,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPhase {
    #[default]
    Select,
    /// The engine requested generative planning; the host returns its proposal.
    ResolvePlanner,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub contract_version: u32,
    /// Host-generated identity of this exact task/catalog/evidence snapshot.
    /// The host refuses a reply for another snapshot, including after steering.
    pub snapshot: String,
    #[serde(default)]
    pub locality: Locality,
    pub context: ActionContext,
    pub tools: Vec<ActionTool>,
    #[serde(default)]
    pub plan: ActionPlan,
    #[serde(default)]
    pub phase: ActionPhase,
    /// A fact supplied by the loop; only the engine interprets its policy cap.
    #[serde(default)]
    pub consecutive_gated_steps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOrigin {
    Structured,
    Planner,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ActionVerdict {
    Execute {
        candidate_id: String,
        call: ToolCall,
        origin: ActionOrigin,
        confidence: Option<f64>,
        /// Continuation is opaque to the host; return it with the next evidence.
        continuation: ActionPlan,
    },
    NeedPlanner {
        reason: String,
        /// Engine-owned planning instructions and state; the host only brokers
        /// the request to the chosen generative engine.
        system: String,
        prompt: String,
    },
    /// Explicitly absent operation/disabled plane allows compatibility behavior.
    Disabled,
    /// Invalid input or invalid planner proposals must never dispatch a tool.
    Rejected { reason: String },
}

/// Latest observed health for a configured decision model. No provider error body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionModelHealth {
    pub model: String,
    /// None means a successful provider call; otherwise a static health reason.
    pub issue: Option<String>,
}

/// Content-free selection receipt. Hashes identify complete calls without
/// retaining argument values, evidence, user prose or model reasoning.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActionDiagnostics {
    pub stage: String,
    pub candidates: Vec<ActionCandidateFingerprint>,
    pub tools_without_candidates: usize,
    pub candidates_truncated: bool,
    pub selected_option: Option<String>,
    pub choice_confidence: Option<f64>,
    pub choice_threshold: Option<f64>,
    pub evidence_sufficient: Option<f64>,
    pub evidence_threshold: Option<f64>,
    pub action_applicable: Option<f64>,
    pub applicability_threshold: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionCandidateFingerprint {
    pub id: String,
    pub call_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionResponse {
    #[serde(default)]
    pub diagnostics: ActionDiagnostics,
    /// Every physical attempt, including failed retries and review calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_calls: Vec<crate::telemetry::DecisionModelCall>,
    pub contract_version: u32,
    pub snapshot: String,
    pub verdict: ActionVerdict,
    /// Content-free reason useful for traces, including planner escalation.
    pub reason: String,
    pub model: Option<crate::identity::ModelIdentity>,
    /// Model that checked the exact selected call (may differ after fit routing).
    #[serde(default)]
    pub review_model: Option<crate::identity::ModelIdentity>,
    #[serde(default)]
    pub usage: Option<crate::request::Usage>,
    pub latency_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_health: Vec<ActionModelHealth>,
}

impl ActionResponse {
    pub fn rejected(snapshot: impl Into<String>, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            diagnostics: ActionDiagnostics::default(),
            contract_version: CONTRACT_VERSION,
            model_calls: Vec::new(),
            snapshot: snapshot.into(),
            verdict: ActionVerdict::Rejected {
                reason: reason.clone(),
            },
            reason,
            model: None,
            review_model: None,
            usage: None,
            latency_ms: 0,
            model_health: Vec::new(),
        }
    }

    pub fn matches(&self, request: &ActionRequest) -> bool {
        self.contract_version == request.contract_version && self.snapshot == request.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_action_responses_without_diagnostics_remain_readable() {
        let response = ActionResponse::rejected("snapshot", "not_authorized");
        let mut wire = serde_json::to_value(&response).unwrap();
        wire.as_object_mut().unwrap().remove("diagnostics");
        let decoded: ActionResponse = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded, response);
    }
}
