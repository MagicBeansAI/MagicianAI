use serde_json::Value;

use crate::types::{LLMRequest, LLMResponse};

use super::{
    ChunkBudget, ChunkDescriptor, ChunkError, ChunkValidationError, FinalValidationContract,
    LogicalItem, LogicalLlmRequest, ReductionContext, ReductionPlan, ReductionRequest,
    TokenEstimator, ValidatedChunkOutput,
};

/// Operation-owned structured split/render/validate/reduce contract.
///
/// Planning is usable without a runner. Execution hooks deliberately default
/// to typed unavailable errors so the Phase 3 runner can reject incomplete
/// concrete adapters without inventing generic domain behavior.
pub trait ChunkDomainAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn supported_operations(&self) -> &'static [&'static str];
    fn final_validation_contract(&self) -> FinalValidationContract;

    fn logical_items(&self, input: &Value) -> Result<Vec<LogicalItem>, ChunkError>;

    fn estimate_item_tokens(
        &self,
        item: &LogicalItem,
        estimator: &dyn TokenEstimator,
        model: &str,
    ) -> Result<u32, ChunkError> {
        Ok(estimator.estimate_text(model, &item.value.to_string()))
    }

    /// Optional semantic fan-in ceiling for one physical map request.
    ///
    /// Token budgets remain the hard context boundary. Adapters whose output
    /// contract scales with source count can additionally keep each request
    /// small enough for the model to return one complete structured result per
    /// source item. `None` preserves the token-only planner behavior.
    fn max_items_per_chunk(&self) -> Option<usize> {
        None
    }

    fn split_oversized_item(
        &self,
        item: &LogicalItem,
        budget: &ChunkBudget,
    ) -> Result<Vec<LogicalItem>, ChunkError>;

    fn render_map_request(
        &self,
        _base: &LogicalLlmRequest,
        _items: &[LogicalItem],
        _chunk: &ChunkDescriptor,
    ) -> Result<LLMRequest, ChunkError> {
        Err(ChunkError::ExecutionHookUnavailable {
            adapter: self.id().to_string(),
            hook: "render_map_request",
        })
    }

    fn parse_and_validate_map_output(
        &self,
        _response: &LLMResponse,
        _chunk: &ChunkDescriptor,
    ) -> Result<Value, ChunkValidationError> {
        Err(ChunkValidationError::HookUnavailable {
            adapter: self.id().to_string(),
            hook: "parse_and_validate_map_output",
        })
    }

    /// Validate an adapter-produced map value, including deterministic
    /// fallback output that did not originate in an `LLMResponse`.
    fn validate_map_value(
        &self,
        value: &Value,
        chunk: &ChunkDescriptor,
    ) -> Result<(), ChunkValidationError>;

    /// Render the single bounded semantic-repair call allowed for an invalid
    /// map response. `None` declares that this adapter has no repair prompt.
    fn render_repair_request(
        &self,
        _base: &LogicalLlmRequest,
        _items: &[LogicalItem],
        _chunk: &ChunkDescriptor,
        _invalid_response: &LLMResponse,
        _validation_error: &ChunkValidationError,
    ) -> Result<Option<LLMRequest>, ChunkError> {
        Ok(None)
    }

    /// Deterministic per-chunk fallback used only by the corresponding policy.
    fn deterministic_fallback(
        &self,
        _items: &[LogicalItem],
        _chunk: &ChunkDescriptor,
        _validation_error: &ChunkValidationError,
    ) -> Result<Option<Value>, ChunkError> {
        Ok(None)
    }

    fn reduce(
        &self,
        _outputs: Vec<ValidatedChunkOutput>,
        _context: &ReductionContext,
    ) -> Result<ReductionPlan, ChunkError> {
        Err(ChunkError::ExecutionHookUnavailable {
            adapter: self.id().to_string(),
            hook: "reduce",
        })
    }

    fn parse_and_validate_reduction_output(
        &self,
        response: &LLMResponse,
        request: &ReductionRequest,
        _context: &ReductionContext,
    ) -> Result<Value, ChunkValidationError> {
        self.parse_and_validate_map_output(response, &request.chunk)
    }

    fn validate_final(&self, value: &Value) -> Result<(), ChunkValidationError>;

    fn serialize_final(&self, value: &Value) -> Result<String, ChunkError> {
        match value {
            Value::String(text) => Ok(text.clone()),
            _ => serde_json::to_string(value).map_err(|error| ChunkError::Adapter {
                adapter: self.id().to_string(),
                reason: format!("failed to serialize final value: {error}"),
            }),
        }
    }
}
