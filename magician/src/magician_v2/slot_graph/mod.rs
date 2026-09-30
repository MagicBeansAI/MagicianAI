pub use magician_core::slot_graph::{enrichment, types};

pub mod adapters;
pub mod elicitation;
pub mod extraction;
pub mod repository;
pub mod rewriter;

pub use adapters::{
    DeterministicExtractionLlm, DeterministicRewriteModel, MultiLlmExtractionAdapter,
    MultiLlmRewriteModel,
};
pub use elicitation::{
    ConfidenceBoostResult, ElicitationConfig, ElicitationResult, ElicitationService,
    SlotGraphRepository, SlotTriggerMapping, TriggeredSlot, STAGE_ELICITATION_OUTCOME,
    STAGE_ELICITATION_REWRITE, STAGE_ELICITATION_SLOTS,
};
pub use enrichment::{
    EnrichmentContext, EnrichmentError, EnrichmentOutcome, EnrichmentPipeline, EnrichmentSummary,
    SlotEnricher,
};
pub use extraction::{
    ConversationContext, ExtractionConfig, LlmFunctionCallRequest, LlmFunctionCallResponse,
    LlmService, Message, MessageRole, Screenshot, SlotExtractor,
};
pub use repository::ConversationSlotGraphRepository;
pub use rewriter::{
    ClarifiedTask, CuratedClarificationQuestion, QuestionRewriter, RewriteModel,
    RewriteModelRequest, RewriteModelResponse, RewriteStrategy, RewriterConfig,
};
pub use types::{ProvenanceRecord, ProvenanceSource, ProvisionalSlot, SlotRecord, SlotType};
