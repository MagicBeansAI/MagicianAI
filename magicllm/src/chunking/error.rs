use thiserror::Error;

/// Typed failures raised while budgeting a physical or logical LLM request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    /// Static prompt, output reservation, and safety margin leave no payload
    /// capacity in one physical provider call.
    #[error(
        "static context exceeds physical window: overhead={estimated_static_overhead_tokens}, \
         reserved_output={reserved_output_tokens}, safety_margin={safety_margin_tokens}, \
         window={context_window_tokens}"
    )]
    StaticContextWindowExceeded {
        estimated_static_overhead_tokens: u32,
        reserved_output_tokens: u32,
        safety_margin_tokens: u32,
        context_window_tokens: u32,
    },
    /// A complete physical request would exceed the configured model window.
    #[error(
        "physical context exceeds model window: estimated_input={estimated_input_tokens}, \
         reserved_output={reserved_output_tokens}, safety_margin={safety_margin_tokens}, \
         required={required_tokens}, window={context_window_tokens}"
    )]
    PhysicalContextWindowExceeded {
        estimated_input_tokens: u32,
        reserved_output_tokens: u32,
        safety_margin_tokens: u32,
        required_tokens: u64,
        context_window_tokens: u32,
    },
    /// A logical operation exceeds the configured end-to-end input window.
    #[error(
        "logical context exceeds configured window: estimated={estimated_logical_tokens}, \
         window={logical_window_tokens}"
    )]
    LogicalContextWindowExceeded {
        estimated_logical_tokens: u32,
        logical_window_tokens: u32,
    },
}

/// Typed failures while deriving a structured logical plan.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChunkError {
    #[error(transparent)]
    Context(#[from] ContextError),
    #[error("adapter `{adapter}` does not support operation `{operation}`")]
    UnsupportedOperation { adapter: String, operation: String },
    #[error("adapter `{adapter}` returned no logical source items")]
    EmptyLogicalInput { adapter: String },
    #[error("invalid logical item identity `{item_id}`: {reason}")]
    InvalidItemIdentity { item_id: String, reason: String },
    #[error("duplicate logical item identity `{item_id}`")]
    DuplicateItemIdentity { item_id: String },
    #[error(
        "chunk item exceeds context window: adapter={adapter}, item={item_id}, estimated={estimated_tokens}, effective_payload={effective_payload_tokens}"
    )]
    ChunkItemExceedsContextWindow {
        adapter: String,
        item_id: String,
        estimated_tokens: u32,
        effective_payload_tokens: u32,
    },
    #[error("adapter `{adapter}` returned no children while splitting item `{item_id}`")]
    EmptyItemSplit { adapter: String, item_id: String },
    #[error("invalid split of item `{item_id}` by adapter `{adapter}`: {reason}")]
    InvalidItemSplit {
        adapter: String,
        item_id: String,
        reason: String,
    },
    #[error("recursive split depth exceeded for item `{item_id}` in adapter `{adapter}`")]
    SplitDepthExceeded { adapter: String, item_id: String },
    #[error("chunk plan is incomplete; missing leaf identities: {item_ids:?}")]
    MissingPlannedItems { item_ids: Vec<String> },
    #[error("chunk plan contains unexpected leaf identities: {item_ids:?}")]
    UnexpectedPlannedItems { item_ids: Vec<String> },
    #[error("chunk plan duplicates leaf identities: {item_ids:?}")]
    DuplicatePlannedItems { item_ids: Vec<String> },
    #[error("chunk plan identity metadata does not match terminal items: {item_ids:?}")]
    MismatchedPlannedItems { item_ids: Vec<String> },
    #[error("chunk plan changes stable source order")]
    InvalidPlannedOrder,
    #[error("invalid chunk descriptor at index {chunk_index}: {reason}")]
    InvalidChunkDescriptor { chunk_index: u32, reason: String },
    #[error("chunk plan is missing source-root coverage: {item_ids:?}")]
    MissingSourceCoverage { item_ids: Vec<String> },
    #[error("adapter `{adapter}` planning failed: {reason}")]
    Adapter { adapter: String, reason: String },
    #[error("adapter `{adapter}` execution hook `{hook}` is not implemented")]
    ExecutionHookUnavailable { adapter: String, hook: &'static str },
}

/// Typed schema/semantic validation failures for map and final outputs.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChunkValidationError {
    #[error("adapter `{adapter}` validation failed: {reason}")]
    InvalidOutput { adapter: String, reason: String },
    #[error("adapter `{adapter}` validation hook `{hook}` is not implemented")]
    HookUnavailable { adapter: String, hook: &'static str },
}
