use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{LLMRequest, LLMResponse};

use super::{calculate_effective_payload, ContextError};

/// Stable identity for one owned logical source item or one semantic child.
///
/// `root_id` always points at the original source identity. Split children
/// record their immediate parent and an ordered path, allowing retries and
/// boundary changes to remain auditable without treating children as new
/// source records.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogicalItemIdentity {
    pub id: String,
    pub root_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub source_order: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segment_path: Vec<u32>,
}

impl LogicalItemIdentity {
    pub fn root(id: impl Into<String>, source_order: u32) -> Self {
        let id = id.into();
        Self {
            root_id: id.clone(),
            id,
            parent_id: None,
            source_order,
            segment_path: Vec::new(),
        }
    }

    pub fn child(id: impl Into<String>, parent: &LogicalItemIdentity, segment_order: u32) -> Self {
        let mut segment_path = parent.segment_path.clone();
        segment_path.push(segment_order);
        Self {
            id: id.into(),
            root_id: parent.root_id.clone(),
            parent_id: Some(parent.id.clone()),
            source_order: parent.source_order,
            segment_path,
        }
    }
}

/// One complete structured unit owned by the logical operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicalItem {
    pub identity: LogicalItemIdentity,
    pub value: Value,
}

impl LogicalItem {
    pub fn root(id: impl Into<String>, source_order: u32, value: Value) -> Self {
        Self {
            identity: LogicalItemIdentity::root(id, source_order),
            value,
        }
    }

    pub fn child(
        id: impl Into<String>,
        parent: &LogicalItem,
        segment_order: u32,
        value: Value,
    ) -> Self {
        Self {
            identity: LogicalItemIdentity::child(id, &parent.identity, segment_order),
            value,
        }
    }
}

/// One caller-visible operation and the structured value its adapter owns.
#[derive(Debug, Clone)]
pub struct LogicalLlmRequest {
    pub operation: String,
    pub input: Value,
    /// Existing physical request template used by later render/execution
    /// phases. Phase 2 reads only its model identity while planning.
    pub base_request: LLMRequest,
}

impl Drop for LogicalLlmRequest {
    fn drop(&mut self) {
        // Logical inputs may be assembled from externally influenced memory or
        // evidence trees. Runner rejection (missing adapter, invalid plan,
        // cancellation) must not hand their recursive destruction back to the
        // native call stack.
        crate::types::discard_json_value_iteratively(std::mem::replace(
            &mut self.input,
            Value::Null,
        ));
    }
}

/// Complete budget for planning one logical operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkBudget {
    pub physical_window_tokens: u32,
    pub logical_window_tokens: u32,
    pub target_payload_tokens: u32,
    pub effective_payload_tokens: u32,
    pub estimated_static_overhead_tokens: u32,
    pub reserved_output_tokens: u32,
    pub safety_margin_tokens: u32,
}

impl ChunkBudget {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        physical_window_tokens: u32,
        logical_window_tokens: u32,
        target_payload_tokens: u32,
        estimated_static_overhead_tokens: u32,
        reserved_output_tokens: u32,
        safety_margin_tokens: u32,
    ) -> Result<Self, ContextError> {
        let effective_payload_tokens = calculate_effective_payload(
            target_payload_tokens,
            physical_window_tokens,
            estimated_static_overhead_tokens,
            reserved_output_tokens,
            safety_margin_tokens,
        )?;
        Ok(Self {
            physical_window_tokens,
            logical_window_tokens,
            target_payload_tokens,
            effective_payload_tokens,
            estimated_static_overhead_tokens,
            reserved_output_tokens,
            safety_margin_tokens,
        })
    }
}

/// Reproducible description of one planned physical map call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkDescriptor {
    pub index: u32,
    pub estimated_payload_tokens: u32,
    pub items: Vec<LogicalItemIdentity>,
}

/// Full pre-dispatch plan. It carries both original roots and terminal leaves
/// so completeness can be rechecked immediately before execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkPlan {
    pub adapter_id: String,
    pub adapter_version: String,
    pub operation: String,
    pub model: String,
    pub estimator: String,
    pub estimated_logical_tokens: u32,
    pub budget: ChunkBudget,
    pub source_identities: Vec<LogicalItemIdentity>,
    pub leaf_identities: Vec<LogicalItemIdentity>,
    /// In-memory terminal payloads for Phase 3 request rendering. The plan is
    /// intentionally not serializable so sensitive source values cannot be
    /// mistaken for telemetry; descriptors carry the safe identity metadata.
    pub leaf_items: Vec<LogicalItem>,
    pub chunks: Vec<ChunkDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalValidationContract {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReductionStrategy {
    Deterministic,
    HierarchicalLlm,
}

#[derive(Debug, Clone)]
pub struct ValidatedChunkOutput {
    pub chunk: ChunkDescriptor,
    pub value: Value,
    pub response: Option<LLMResponse>,
}

#[derive(Debug, Clone)]
pub struct ReductionContext {
    pub operation: String,
    pub adapter_id: String,
    pub adapter_version: String,
    pub source_identities: Vec<LogicalItemIdentity>,
    pub budget: ChunkBudget,
    /// Original physical request template. Hierarchical adapters replace only
    /// messages/content while retaining the selected model and generation
    /// settings; the runner still applies its exact-profile/provider lock.
    pub base_request: LLMRequest,
}

/// One budgeted request in a hierarchical reduction level. The descriptor
/// carries source membership separately from model-authored content.
#[derive(Debug, Clone)]
pub struct ReductionRequest {
    pub chunk: ChunkDescriptor,
    pub request: LLMRequest,
}

/// A reducer either completes locally or asks Phase 3's runner for another
/// budgeted physical request. The runner owns hierarchical execution.
#[derive(Debug, Clone)]
pub enum ReductionPlan {
    Complete(Value),
    PhysicalRequests {
        strategy: ReductionStrategy,
        requests: Vec<ReductionRequest>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_request_releases_deep_input_iteratively_on_rejection() {
        std::thread::Builder::new()
            .name("logical-request-drop-regression".to_string())
            .stack_size(96 * 1024)
            .spawn(|| {
                let mut deep = Value::Null;
                for _ in 0..16_384 {
                    deep = Value::Array(vec![deep]);
                }
                drop(LogicalLlmRequest {
                    operation: "rejected-before-planning".to_string(),
                    input: deep,
                    base_request: LLMRequest::default(),
                });
            })
            .expect("small-stack regression thread")
            .join()
            .expect("logical input destruction must not overflow");
    }
}
