//! Storage abstractions shared across runtime crates and Magician V2.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Pagination parameters (limit + offset).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginationParams {
    pub limit: usize,
    pub offset: usize,
}

impl Default for PaginationParams {
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
        }
    }
}

impl PaginationParams {
    /// Construct with optional limit/offset while enforcing sane defaults.
    pub fn new(limit: Option<usize>, offset: Option<usize>) -> Self {
        const MAX_LIMIT: usize = 200;
        let limit = limit.unwrap_or(50).min(MAX_LIMIT);
        let offset = offset.unwrap_or(0);
        Self { limit, offset }
    }
}

/// Pagination metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginationInfo {
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
}

/// Generic paginated response wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedResult<T> {
    pub items: Vec<T>,
    pub pagination: PaginationInfo,
}

impl<T> PaginatedResult<T> {
    pub fn new(items: Vec<T>, pagination: PaginationInfo) -> Self {
        Self { items, pagination }
    }
}

/// The authority of the work a run belongs to, as it crosses the storage
/// boundary.
///
/// The runtime's own carrier is a typed work context, but this crate cannot
/// depend on the crate that owns work contexts, so the pair crosses as a kind
/// token plus an id. Three named fields rather than a tuple: a store that
/// swapped the kind and the id would persist a run confined to a work context
/// nobody granted, and positional arguments are how that happens.
///
/// The token is *not* re-interpreted here. The magician-side store rebuilds
/// the typed kind from it and refuses a token it does not know, so an
/// unrecognised kind fails closed instead of being coerced into the nearest
/// arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkAuthorityGrant {
    /// Which kind of work granted the authority (`engagement`, `program`, …).
    pub work_kind: String,
    /// The id of that work.
    pub work_id: String,
    /// The revision of the grant the run was admitted under, as read from the
    /// roster that owns the work — never from the caller.
    pub authority_revision: u64,
}

/// Trait describing the Magician V2 conversation store boundary.
#[async_trait]
pub trait V2ConversationStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;
    type Execution: Send + Sync + Clone;
    type ExecutionSummary: Send + Sync + Clone;
    type ExecutionStatus: Send + Sync + Clone + 'static;
    type Turn: Send + Sync + Clone;
    type TurnDirection: Send + Sync + Clone + 'static;
    type Slot: Send + Sync + Clone;
    type SlotStatus: Send + Sync + Clone + 'static;
    type StrategyAttempt: Send + Sync + Clone;
    type ProcessingMetadata: Send + Sync + Clone;
    type UnifiedAnalysis: Send + Sync + Clone;
    type AnalysisMetadata: Send + Sync + Clone;
    type StateBundle: Send + Sync + Clone;
    type RecommendedQuestion: Send + Sync + Clone;
    type PendingClarification: Send + Sync + Clone;
    type ClarificationSession: Send + Sync + Clone;
    type ClarificationHistoryEntry: Send + Sync + Clone;

    async fn create_execution_with_options(
        &self,
        principal: &str,
        workspace: &str,
        title: Option<String>,
        active_owner_agent_id: &str,
        task_id: Option<String>,
        root_execution_id: Option<String>,
        execution_id: Option<String>,
        parent_execution_id: Option<String>,
        timeout_secs: Option<u64>,
        delegation_chain: Vec<String>,
        waiting_state: Self::ExecutionStatus,
    ) -> std::result::Result<Self::Execution, Self::Error>;

    /// [`Self::create_execution_with_options`] carrying the authority of the
    /// **work** that asked for the run, to persist on the created execution
    /// record.
    ///
    /// Takes a [`WorkAuthorityGrant`] rather than an engagement id: the record
    /// this writes into is generic, and a second flow — support triage,
    /// recruiting, vendor management — must be able to confine a run without
    /// inventing an engagement to name.
    ///
    /// # Required, so the grant cannot be dropped invisibly
    ///
    /// This was once a provided method whose default delegated to the plain
    /// variant and threw the grant away. An implementor got that by writing
    /// nothing at all, so the drop appeared in no diff and no review. It is
    /// required now: every store says in its own body what it does with the
    /// carrier, and a store that drops it drops it on a visible line.
    ///
    /// That does not relieve the caller. A store may still legitimately drop
    /// the carrier — a test double with no record to write it onto does — so
    /// every caller **must compare the returned record against what it asked
    /// for**. The record is the answer, and a store that dropped the carrier
    /// says so in the record it returns. `MagicianV2Orchestrator` does exactly
    /// that on both write paths, and refuses rather than running a child that
    /// escaped its ceiling.
    #[allow(clippy::too_many_arguments)]
    async fn create_execution_with_work_authority(
        &self,
        principal: &str,
        workspace: &str,
        title: Option<String>,
        active_owner_agent_id: &str,
        task_id: Option<String>,
        root_execution_id: Option<String>,
        execution_id: Option<String>,
        parent_execution_id: Option<String>,
        timeout_secs: Option<u64>,
        delegation_chain: Vec<String>,
        waiting_state: Self::ExecutionStatus,
        work_authority: Option<WorkAuthorityGrant>,
    ) -> std::result::Result<Self::Execution, Self::Error>;

    async fn update_execution_entry_mode(
        &self,
        execution_id: &str,
        entry_mode: String,
    ) -> std::result::Result<(), Self::Error>;

    async fn get_execution(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Self::Execution, Self::Error>;

    async fn add_child_execution_id(
        &self,
        parent_execution_id: &str,
        child_execution_id: &str,
    ) -> std::result::Result<(), Self::Error>;

    async fn add_turn(
        &self,
        execution_id: &str,
        direction: Self::TurnDirection,
        text: String,
        in_reply_to_slot_id: Option<String>,
    ) -> std::result::Result<Self::Turn, Self::Error>;

    /// Idempotently append one server-derived turn identity. `None` means this
    /// store cannot provide exact deterministic insertion; the default is a
    /// no-mutation refusal so recovery can fail closed without appending a
    /// random duplicate turn.
    async fn add_turn_with_id(
        &self,
        execution_id: &str,
        _turn_id: &str,
        direction: Self::TurnDirection,
        text: String,
        in_reply_to_slot_id: Option<String>,
    ) -> Option<std::result::Result<Self::Turn, Self::Error>> {
        let _ = (execution_id, direction, text, in_reply_to_slot_id);
        None
    }

    async fn store_analysis(
        &self,
        execution_id: &str,
        turn_id: &str,
        analysis: Self::UnifiedAnalysis,
        metadata: Self::AnalysisMetadata,
    ) -> std::result::Result<(), Self::Error>;

    async fn store_strategy_attempts(
        &self,
        execution_id: &str,
        turn_id: &str,
        attempts: Vec<Self::StrategyAttempt>,
        processing_metadata: Self::ProcessingMetadata,
        recommended_questions: Option<Vec<Self::RecommendedQuestion>>,
    ) -> std::result::Result<(), Self::Error>;

    async fn get_turn(
        &self,
        execution_id: &str,
        turn_id: &str,
    ) -> std::result::Result<Self::Turn, Self::Error>;

    async fn get_turns(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Vec<Self::Turn>, Self::Error>;

    async fn get_latest_turn_with_analysis(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Option<Self::Turn>, Self::Error>;

    async fn list_executions(
        &self,
        principal: &str,
        workspace: &str,
        pagination: PaginationParams,
    ) -> std::result::Result<PaginatedResult<Self::ExecutionSummary>, Self::Error>;

    async fn delete_execution(&self, execution_id: &str) -> std::result::Result<(), Self::Error>;

    async fn get_turns_paginated(
        &self,
        execution_id: &str,
        pagination: PaginationParams,
        direction_filter: Option<Self::TurnDirection>,
    ) -> std::result::Result<PaginatedResult<Self::Turn>, Self::Error>;

    async fn update_execution_status(
        &self,
        execution_id: &str,
        status: Self::ExecutionStatus,
    ) -> std::result::Result<(), Self::Error>;

    /// Atomically replace one exact status generation when the backing store
    /// supports cross-process execution locking. Stores that do not implement
    /// a real compare-and-swap must fail closed: silently falling back to an
    /// unconditional write can resurrect a terminal execution chosen by a
    /// concurrent owner.
    async fn compare_exchange_execution_status(
        &self,
        execution_id: &str,
        expected: Self::ExecutionStatus,
        status: Self::ExecutionStatus,
    ) -> std::result::Result<bool, Self::Error>;

    /// Same compare-and-swap as [`V2ConversationStore::compare_exchange_execution_status`],
    /// for a caller that already holds this execution's task-scoped
    /// cross-process lock as an outer transaction fence.
    ///
    /// `flock` is per open-file-description, so a store whose ordinary CAS
    /// takes that same task lock would open a second descriptor here and block
    /// against its own caller forever. Such stores MUST override this to reuse
    /// the caller's fence instead of re-acquiring it; the exclusion is
    /// unchanged because the caller already holds the exact same lock. Stores
    /// without cross-process locking inherit the default delegation.
    async fn compare_exchange_execution_status_holding_task_lock(
        &self,
        execution_id: &str,
        expected: Self::ExecutionStatus,
        status: Self::ExecutionStatus,
    ) -> std::result::Result<bool, Self::Error> {
        self.compare_exchange_execution_status(execution_id, expected, status)
            .await
    }

    /// Atomically replace a status only when both its enum and status-only
    /// generation match. Multi-record protocols use this stronger form to
    /// reject Paused -> active ABA after a newer pause was published.
    async fn compare_exchange_execution_status_at(
        &self,
        execution_id: &str,
        expected: Self::ExecutionStatus,
        expected_status_revision: u64,
        status: Self::ExecutionStatus,
    ) -> std::result::Result<bool, Self::Error>;

    /// Read the status-only generation paired with `get_execution`. Metadata
    /// mutations must not change this value.
    async fn get_execution_status_revision(
        &self,
        execution_id: &str,
    ) -> std::result::Result<u64, Self::Error>;

    async fn bind_execution_scope(
        &self,
        execution_id: &str,
        task_id: Option<String>,
        root_execution_id: Option<String>,
    ) -> std::result::Result<(), Self::Error>;

    async fn update_execution_owner_snapshot(
        &self,
        execution_id: &str,
        active_owner_agent_id: &str,
        owner_stack: &[String],
    ) -> std::result::Result<(), Self::Error>;

    async fn replace_active_delegation_group(
        &self,
        execution_id: &str,
        active_delegation_group: &[String],
    ) -> std::result::Result<(), Self::Error>;

    /// Atomically settle a delegation parent and clear the active child group.
    ///
    /// Stores that can update one execution document transactionally should
    /// override this method. The default preserves compatibility for embedded
    /// and test stores, while the canonical file store provides the atomic
    /// implementation used by production recovery paths.
    async fn settle_delegation_parent(
        &self,
        execution_id: &str,
        status: Self::ExecutionStatus,
    ) -> std::result::Result<(), Self::Error> {
        self.update_execution_status(execution_id, status).await?;
        self.replace_active_delegation_group(execution_id, &[])
            .await
    }

    async fn get_execution_status(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Self::ExecutionStatus, Self::Error>;

    async fn update_processing_correlation_id(
        &self,
        execution_id: &str,
        correlation_id: Option<String>,
    ) -> std::result::Result<(), Self::Error>;

    async fn create_slot(
        &self,
        execution_id: &str,
        name: String,
        schema_json: serde_json::Value,
        required: bool,
        asked_turn_id: Option<String>,
    ) -> std::result::Result<Self::Slot, Self::Error>;

    async fn update_slot_answer(
        &self,
        execution_id: &str,
        slot_id: &str,
        answer: serde_json::Value,
    ) -> std::result::Result<(), Self::Error>;

    async fn update_slot_status(
        &self,
        execution_id: &str,
        slot_id: &str,
        status: Self::SlotStatus,
    ) -> std::result::Result<(), Self::Error>;

    async fn get_slots(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Vec<Self::Slot>, Self::Error>;

    async fn get_pending_slots(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Vec<Self::Slot>, Self::Error>;

    async fn get_slot(
        &self,
        execution_id: &str,
        slot_id: &str,
    ) -> std::result::Result<Self::Slot, Self::Error>;

    async fn append_state(
        &self,
        execution_id: &str,
        state: Self::StateBundle,
    ) -> std::result::Result<(), Self::Error>;

    async fn get_states(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Vec<Self::StateBundle>, Self::Error>;

    async fn get_latest_state(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Option<Self::StateBundle>, Self::Error>;

    async fn update_execution_processing_stage(
        &self,
        execution_id: &str,
        stage: Option<String>,
        provider: Option<String>,
    ) -> std::result::Result<(), Self::Error>;

    async fn update_turn_enriched_query(
        &self,
        execution_id: &str,
        turn_id: &str,
        enriched_query: Option<String>,
    ) -> std::result::Result<(), Self::Error>;

    async fn load_clarification_session(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Option<Self::ClarificationSession>, Self::Error>;

    async fn store_clarification_session(
        &self,
        execution_id: &str,
        session: Self::ClarificationSession,
    ) -> std::result::Result<(), Self::Error>;

    async fn delete_clarification_session(
        &self,
        execution_id: &str,
    ) -> std::result::Result<(), Self::Error>;

    async fn append_clarification_history(
        &self,
        execution_id: &str,
        entries: Vec<Self::ClarificationHistoryEntry>,
    ) -> std::result::Result<(), Self::Error>;

    async fn get_clarification_history(
        &self,
        execution_id: &str,
    ) -> std::result::Result<Vec<Self::ClarificationHistoryEntry>, Self::Error>;
}
