// Stage 1: Query Understanding & Analysis
// Parse and understand user queries to extract intent, complexity, and target
// categories

pub mod intent;
pub mod llm_eval;
pub mod multi_llm_service;
pub mod operation_llm_router;
pub mod parent_engine;
pub mod unified_analyzer;

// Re-export key types and traits for unified analysis
// Re-export intent classification types
pub use intent::{QueryIntent, SlotMatchAnalysis};
// Re-export evaluation framework for testing
pub use llm_eval::{
    EvaluationCase, EvaluationDataset, EvaluationMetrics, EvaluationResult, LLMEvaluator,
};
pub use unified_analyzer::{
    CategoryAnalysis, ComplexityAnalysis, ConversationContext, DependencyAnalysis,
    ExtractedEntities, ResourceEstimate, TaskClarity, UnifiedQueryAnalysis, UnifiedQueryAnalyzer,
};
