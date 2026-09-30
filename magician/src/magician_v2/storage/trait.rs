//! Type-safe alias bridging the core V2 conversation store trait.

use async_trait::async_trait;
use runtime_core::V2ConversationStore as CoreV2ConversationStore;
use thiserror::Error;

use super::models::{
    ClarificationHistoryEntry, ExecutionRun, ExecutionSummary, StrategyAttempt, TurnDirection,
    V2Slot, V2SlotStatus, V2Turn, WaitingState,
};
use crate::magician_v2::{
    ask_loop::ClarificationSession,
    orchestrator::v2_orchestrator::{
        PendingClarification, ProcessingMetadata, RecommendedQuestion,
    },
    state_tracker::StateBundle,
    AnalysisMetadata, UnifiedQueryAnalysis,
};

#[derive(Debug, Error)]
pub enum V2StorageError {
    #[error("Execution not found: {0}")]
    ExecutionNotFound(String),

    #[error("Turn not found: {0}")]
    TurnNotFound(String),

    #[error("Slot not found: {0}")]
    SlotNotFound(String),

    #[error("State not found: {0}")]
    StateNotFound(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience trait tying the core storage boundary to concrete V2 types.
#[async_trait]
pub trait V2ConversationStore:
    CoreV2ConversationStore<
        Error = V2StorageError,
        Execution = ExecutionRun,
        ExecutionSummary = ExecutionSummary,
        ExecutionStatus = WaitingState,
        Turn = V2Turn,
        TurnDirection = TurnDirection,
        Slot = V2Slot,
        SlotStatus = V2SlotStatus,
        StrategyAttempt = StrategyAttempt,
        ProcessingMetadata = ProcessingMetadata,
        UnifiedAnalysis = UnifiedQueryAnalysis,
        AnalysisMetadata = AnalysisMetadata,
        StateBundle = StateBundle,
        RecommendedQuestion = RecommendedQuestion,
        PendingClarification = PendingClarification,
        ClarificationSession = ClarificationSession,
        ClarificationHistoryEntry = ClarificationHistoryEntry,
    > + Send
{
    async fn append_clarification_history(
        &self,
        execution_id: &str,
        entries: Vec<ClarificationHistoryEntry>,
    ) -> Result<(), V2StorageError>;
}

#[async_trait]
impl<T> V2ConversationStore for T
where
    T: CoreV2ConversationStore<
            Error = V2StorageError,
            Execution = ExecutionRun,
            ExecutionSummary = ExecutionSummary,
            ExecutionStatus = WaitingState,
            Turn = V2Turn,
            TurnDirection = TurnDirection,
            Slot = V2Slot,
            SlotStatus = V2SlotStatus,
            StrategyAttempt = StrategyAttempt,
            ProcessingMetadata = ProcessingMetadata,
            UnifiedAnalysis = UnifiedQueryAnalysis,
            AnalysisMetadata = AnalysisMetadata,
            StateBundle = StateBundle,
            RecommendedQuestion = RecommendedQuestion,
            PendingClarification = PendingClarification,
            ClarificationSession = ClarificationSession,
            ClarificationHistoryEntry = ClarificationHistoryEntry,
        > + Send,
{
    async fn append_clarification_history(
        &self,
        execution_id: &str,
        entries: Vec<ClarificationHistoryEntry>,
    ) -> Result<(), V2StorageError> {
        CoreV2ConversationStore::append_clarification_history(self, execution_id, entries).await
    }
}
