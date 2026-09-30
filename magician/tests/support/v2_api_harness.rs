//! The in-process MagicianV2 API harness shared by the integration test
//! binaries: mock storage/LLM/tool/prompt seams, the orchestrator + API
//! factory (`create_test_v2_api_with_storage_path`, scripted native model
//! turns in debug builds), and the scope/runtime helpers. Included by
//! `#[path]` from each binary that drives the API in-process.
#![allow(dead_code, unused_imports)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use actix_web::{test, web, App, HttpRequest};
use magician::magician_v2::{
    agents::{
        ActionPattern, AgentMemoryService, AgentStorage, ToolActionPattern, TrustLevel,
        TrustPolicyFile,
    },
    artifact_v2::memory::V3EpisodeRecord,
    ask_loop::session_manager::SessionManager,
    execution::{
        agentic::native_types::{ExecutionNativeResponse, ExecutionToolCall},
        ExecutionConfig as MagicutorExecutionConfig, MagicutorClient,
    },
    orchestrator::v2_orchestrator::ProcessingMetadata,
    prompts::PromptManager,
    query_analysis::{
        operation_llm_router::{OperationLlmRouter, QueryAnalysisLLM, SimplifiedLLMResponse},
        UnifiedQueryAnalysis,
    },
    services::MagicianV2Services,
    state_tracker::StateBundle,
    storage::{
        ExecutionEntryMode, ExecutionRun, ExecutionRunDocument, ExecutionSummary, StrategyAttempt,
        TurnDirection, V2ConversationStore, V2Slot, V2SlotStatus, V2StorageError, V2Turn,
        WaitingState,
    },
    AnalysisMetadata, MagicianV2Orchestrator,
};
use magician_api::{
    web_api::{get_approval_handler, list_approvals_handler, respond_hitl_handler},
    {
        create_agent_definition_handler, delete_agent_definition_handler,
        get_agent_definition_handler, list_agent_definitions_handler, manual_trigger_agent_handler,
        pause_agent_handler, resume_agent_handler, update_agent_definition_handler, MagicianV2Api,
    },
};

use runtime_core::{
    ExecutionContext, MultipleToolMatchResult, PaginatedResult, PaginationInfo, PaginationParams,
    RuntimeConfig, ToolCatalog, ToolDiscovery, ToolMatchResult, ToolMatching,
    V2ConversationStore as CoreV2ConversationStore,
};
use serde_json::{json, Value};
use tokio::sync::Mutex;

pub const INTEGRATION_PRINCIPAL: &str = "integration-user";
pub const INTEGRATION_WORKSPACE: &str = "integration-workspace";

pub fn with_integration_scope(request: test::TestRequest) -> test::TestRequest {
    request
        .insert_header(("X-Principal", INTEGRATION_PRINCIPAL))
        .insert_header(("X-Workspace", INTEGRATION_WORKSPACE))
}

// ============================================================================
// Mock Storage Implementation for Testing
// ============================================================================

#[derive(Clone)]
pub struct MockV2Store {
    executions: Arc<Mutex<HashMap<String, ExecutionRunDocument>>>,
}

impl MockV2Store {
    pub fn new() -> Self {
        Self {
            executions: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

pub async fn create_task_backed_execution(
    store: &Arc<dyn V2ConversationStore>,
    principal: &str,
    workspace: &str,
    title: Option<String>,
) -> ExecutionRun {
    let execution_id = uuid::Uuid::new_v4().to_string();
    let task_id = uuid::Uuid::new_v4().to_string();
    store
        .create_execution_with_options(
            principal,
            workspace,
            title,
            magician::magician_v2::chat::DEFAULT_AGENT_ID,
            Some(task_id),
            Some(execution_id.clone()),
            Some(execution_id),
            None,
            None,
            Vec::new(),
            WaitingState::Planning,
        )
        .await
        .expect("task-backed execution created")
}

#[tokio::test]
pub async fn mock_v2_store_update_execution_status_persists_waiting_state() {
    let store = MockV2Store::new();
    let execution = store
        .create_execution_with_options(
            "user",
            "workspace",
            Some("Mock status persistence".to_string()),
            magician::magician_v2::chat::DEFAULT_AGENT_ID,
            Some("task-mock-status".to_string()),
            Some("exec-mock-status".to_string()),
            Some("exec-mock-status".to_string()),
            None,
            None,
            Vec::new(),
            WaitingState::Planning,
        )
        .await
        .expect("execution should be created");

    store
        .update_execution_status(&execution.id, WaitingState::Paused)
        .await
        .expect("status write should persist");

    let reloaded = store
        .get_execution(&execution.id)
        .await
        .expect("execution should be readable");
    assert_eq!(reloaded.waiting_state, WaitingState::Paused);
    assert_eq!(reloaded.paused_from_state, Some(WaitingState::Planning));
}

/// Pins: a test double still DROPS a work authority, and the record it hands
/// back is what says so.
///
/// `create_execution_with_work_authority` used to be a provided trait method
/// whose default threw the grant away, so a store inherited that drop by
/// writing nothing and no diff ever showed it. Making the method required moved
/// the drop onto a visible line in every store; it must not have moved the
/// behaviour. Two things fail here if that slipped: a double that quietly began
/// honouring the grant (`work_authority` no longer `None`), or a caller-side
/// comparison that stopped refusing a record whose confinement is not the one
/// that was asked for — the check both production write paths depend on.
#[tokio::test]
pub async fn mock_v2_store_drops_a_work_authority_and_the_returned_record_says_so() {
    let store = MockV2Store::new();
    let requested = magician::magician_v2::work_context::WorkAuthorityRef::new(
        magician::magician_v2::work_context::WorkContextKind::Program("recruiting".to_string()),
        4,
    )
    .expect("a program-scoped carrier is constructible");

    let created = store
        .create_execution_with_work_authority(
            "user",
            "workspace",
            Some("Mock dropped carrier".to_string()),
            magician::magician_v2::chat::DEFAULT_AGENT_ID,
            Some("task-mock-authority".to_string()),
            Some("exec-mock-authority".to_string()),
            Some("exec-mock-authority".to_string()),
            None,
            None,
            Vec::new(),
            WaitingState::Planning,
            magician::magician_v2::orchestrator::v2_orchestrator::work_authority_grant(Some(
                &requested,
            )),
        )
        .await
        .expect("execution should be created");

    assert!(
        created.work_authority.is_none(),
        "this double is documented as dropping the grant; if it silently started \
         honouring one, the doubles would stop exercising the refusal that guards \
         a run whose confinement is not what its record says"
    );

    // Absent is not the same answer as "the confinement I asked for", and the
    // caller is the only place that can tell the difference.
    let refusal =
        magician::magician_v2::orchestrator::v2_orchestrator::verify_persisted_work_authority(
            &created,
            Some(&requested),
        )
        .expect_err("a record that lost the carrier it was asked for must be refused");
    assert!(
        refusal.contains("program:recruiting"),
        "the refusal must name the confinement that was asked for, so an operator \
         can see which ceiling the run escaped; got: {refusal}"
    );

    let reloaded = store
        .get_execution(&created.id)
        .await
        .expect("execution should be readable");
    assert!(
        reloaded.work_authority.is_none(),
        "the drop must be durable too — a carrier that appeared only on reload \
         would mean the returned record was not the answer callers verify against"
    );
}

#[async_trait::async_trait]
impl CoreV2ConversationStore for MockV2Store {
    type Error = V2StorageError;
    type Execution = ExecutionRun;
    type ExecutionSummary = ExecutionSummary;
    type ExecutionStatus = WaitingState;
    type Turn = V2Turn;
    type TurnDirection = TurnDirection;
    type Slot = V2Slot;
    type SlotStatus = V2SlotStatus;
    type StrategyAttempt = StrategyAttempt;
    type ProcessingMetadata = ProcessingMetadata;
    type UnifiedAnalysis = UnifiedQueryAnalysis;
    type AnalysisMetadata = AnalysisMetadata;
    type StateBundle = StateBundle;
    type RecommendedQuestion =
        magician::magician_v2::orchestrator::v2_orchestrator::RecommendedQuestion;
    type PendingClarification =
        magician::magician_v2::orchestrator::v2_orchestrator::PendingClarification;
    type ClarificationSession = magician::magician_v2::ask_loop::ClarificationSession;
    type ClarificationHistoryEntry =
        magician::magician_v2::storage::models::ClarificationHistoryEntry;

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
        waiting_state: WaitingState,
    ) -> Result<ExecutionRun, V2StorageError> {
        let execution = ExecutionRun {
            id: execution_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            task_id,
            root_execution_id,
            title,
            waiting_state,
            created_at: chrono::Utc::now().timestamp_millis(),
            updated_at: chrono::Utc::now().timestamp_millis(),
            processing_correlation_id: None,
            current_stage: None,
            current_provider: None,
            escalation_trigger: None,
            parent_execution_id,
            child_execution_ids: Vec::new(),
            active_owner_agent_id: active_owner_agent_id.to_string(),
            owner_stack: Vec::new(),
            active_delegation_group: Vec::new(),
            timeout_secs,
            delegation_chain,
            work_authority: None,
            paused_from_state: None,
            entry_mode: ExecutionEntryMode::PlanningBacked,
        };

        let doc = ExecutionRunDocument {
            execution: execution.clone(),
            status_revision: 0,
            turns: Vec::new(),
            slots: Vec::new(),
            states: Vec::new(),
            clarification_session: None,
            clarification_history: Vec::new(),
        };

        self.executions
            .lock()
            .await
            .insert(execution.id.clone(), doc);
        Ok(execution)
    }

    // Does not persist a work authority: the record this mock builds fixes
    // `work_authority: None`, so delegating drops the grant exactly as the
    // plain variant does. These API tests pass no grant, and a caller that
    // did would see the drop in the record it got back.
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
        waiting_state: WaitingState,
        _work_authority: Option<runtime_core::WorkAuthorityGrant>,
    ) -> Result<ExecutionRun, V2StorageError> {
        CoreV2ConversationStore::create_execution_with_options(
            self,
            principal,
            workspace,
            title,
            active_owner_agent_id,
            task_id,
            root_execution_id,
            execution_id,
            parent_execution_id,
            timeout_secs,
            delegation_chain,
            waiting_state,
        )
        .await
    }

    async fn update_execution_entry_mode(
        &self,
        execution_id: &str,
        entry_mode: String,
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        doc.execution.entry_mode = ExecutionEntryMode::from_token(&entry_mode);
        doc.execution.updated_at = chrono::Utc::now()
            .timestamp_millis()
            .max(doc.execution.updated_at.saturating_add(1));
        Ok(())
    }

    async fn get_execution(&self, execution_id: &str) -> Result<ExecutionRun, V2StorageError> {
        self.executions
            .lock()
            .await
            .get(execution_id)
            .map(|doc| doc.execution.clone())
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))
    }

    async fn add_turn(
        &self,
        execution_id: &str,
        direction: TurnDirection,
        text: String,
        in_reply_to_slot_id: Option<String>,
    ) -> Result<V2Turn, V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;

        let turn = V2Turn {
            id: uuid::Uuid::new_v4().to_string(),
            execution_id: execution_id.to_string(),
            direction,
            text,
            in_reply_to_slot_id,
            created_at: chrono::Utc::now().timestamp_millis(),
            query_analysis: None,
            analysis_metadata: None,
            strategy_attempts: vec![],
            processing_metadata: None,
            recommended_questions: None,
            enriched_query: None,
        };

        doc.turns.push(turn.clone());
        doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
        Ok(turn)
    }

    async fn store_analysis(
        &self,
        execution_id: &str,
        turn_id: &str,
        analysis: UnifiedQueryAnalysis,
        metadata: AnalysisMetadata,
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;

        let turn = doc
            .turns
            .iter_mut()
            .find(|t| t.id == turn_id)
            .ok_or_else(|| V2StorageError::TurnNotFound(turn_id.to_string()))?;

        turn.query_analysis = Some(analysis);
        turn.analysis_metadata = Some(metadata);
        Ok(())
    }

    async fn get_turn(&self, execution_id: &str, turn_id: &str) -> Result<V2Turn, V2StorageError> {
        self.executions
            .lock()
            .await
            .get(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?
            .turns
            .iter()
            .find(|t| t.id == turn_id)
            .cloned()
            .ok_or_else(|| V2StorageError::TurnNotFound(turn_id.to_string()))
    }

    async fn get_turns(&self, execution_id: &str) -> Result<Vec<V2Turn>, V2StorageError> {
        Ok(self
            .executions
            .lock()
            .await
            .get(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?
            .turns
            .clone())
    }

    async fn get_latest_turn_with_analysis(
        &self,
        execution_id: &str,
    ) -> Result<Option<V2Turn>, V2StorageError> {
        Ok(self
            .executions
            .lock()
            .await
            .get(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?
            .turns
            .iter()
            .rev()
            .find(|t| t.query_analysis.is_some())
            .cloned())
    }

    async fn list_executions(
        &self,
        principal: &str,
        workspace: &str,
        pagination: PaginationParams,
    ) -> Result<PaginatedResult<ExecutionSummary>, V2StorageError> {
        let executions = self.executions.lock().await;
        let mut summaries: Vec<ExecutionSummary> = executions
            .values()
            .filter(|doc| {
                doc.execution.principal == principal && doc.execution.workspace == workspace
            })
            .map(ExecutionSummary::from)
            .collect();

        // Sort by updated_at descending
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        let total = summaries.len();
        let items = summaries
            .into_iter()
            .skip(pagination.offset)
            .take(pagination.limit)
            .collect::<Vec<_>>();

        let has_more = pagination.offset + items.len() < total;

        Ok(PaginatedResult {
            items,
            pagination: PaginationInfo {
                total,
                limit: pagination.limit,
                offset: pagination.offset,
                has_more,
            },
        })
    }

    async fn delete_execution(&self, execution_id: &str) -> Result<(), V2StorageError> {
        self.executions
            .lock()
            .await
            .remove(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        Ok(())
    }

    async fn get_turns_paginated(
        &self,
        execution_id: &str,
        pagination: PaginationParams,
        direction_filter: Option<TurnDirection>,
    ) -> Result<PaginatedResult<V2Turn>, V2StorageError> {
        let executions = self.executions.lock().await;
        let doc = executions
            .get(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;

        let turns: Vec<V2Turn> = if let Some(ref direction) = direction_filter {
            doc.turns
                .iter()
                .filter(|t| &t.direction == direction)
                .cloned()
                .collect()
        } else {
            doc.turns.clone()
        };

        let total = turns.len();
        let items: Vec<V2Turn> = turns
            .into_iter()
            .skip(pagination.offset)
            .take(pagination.limit)
            .collect();

        let has_more = pagination.offset + items.len() < total;

        Ok(PaginatedResult {
            items,
            pagination: PaginationInfo {
                total,
                limit: pagination.limit,
                offset: pagination.offset,
                has_more,
            },
        })
    }

    async fn update_execution_status(
        &self,
        execution_id: &str,
        status: magician::magician_v2::storage::WaitingState,
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        if status == WaitingState::Paused {
            if doc.execution.waiting_state != WaitingState::Paused
                && !doc.execution.waiting_state.is_terminal()
            {
                doc.execution.paused_from_state = Some(doc.execution.waiting_state.clone());
            }
        } else {
            doc.execution.paused_from_state = None;
        }
        doc.execution.waiting_state = status;
        doc.status_revision = doc.status_revision.saturating_add(1);
        doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
        Ok(())
    }

    async fn compare_exchange_execution_status(
        &self,
        execution_id: &str,
        expected: WaitingState,
        status: WaitingState,
    ) -> Result<bool, V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        if doc.execution.waiting_state != expected {
            return Ok(false);
        }
        if status == WaitingState::Paused {
            if doc.execution.waiting_state != WaitingState::Paused
                && !doc.execution.waiting_state.is_terminal()
            {
                doc.execution.paused_from_state = Some(doc.execution.waiting_state.clone());
            }
        } else {
            doc.execution.paused_from_state = None;
        }
        doc.execution.waiting_state = status;
        doc.status_revision = doc.status_revision.saturating_add(1);
        doc.execution.updated_at = chrono::Utc::now()
            .timestamp_millis()
            .max(doc.execution.updated_at.saturating_add(1));
        Ok(true)
    }

    async fn compare_exchange_execution_status_at(
        &self,
        execution_id: &str,
        expected: WaitingState,
        expected_status_revision: u64,
        status: WaitingState,
    ) -> Result<bool, V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        if doc.execution.waiting_state != expected
            || doc.status_revision != expected_status_revision
        {
            return Ok(false);
        }
        if status == WaitingState::Paused {
            if doc.execution.waiting_state != WaitingState::Paused
                && !doc.execution.waiting_state.is_terminal()
            {
                doc.execution.paused_from_state = Some(doc.execution.waiting_state.clone());
            }
        } else {
            doc.execution.paused_from_state = None;
        }
        doc.execution.waiting_state = status;
        doc.status_revision = doc.status_revision.saturating_add(1);
        doc.execution.updated_at = chrono::Utc::now()
            .timestamp_millis()
            .max(doc.execution.updated_at.saturating_add(1));
        Ok(true)
    }

    async fn get_execution_status_revision(
        &self,
        execution_id: &str,
    ) -> Result<u64, V2StorageError> {
        self.executions
            .lock()
            .await
            .get(execution_id)
            .map(|doc| doc.status_revision)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))
    }

    async fn add_child_execution_id(
        &self,
        parent_execution_id: &str,
        child_execution_id: &str,
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(parent_execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(parent_execution_id.to_string()))?;
        if !doc
            .execution
            .child_execution_ids
            .iter()
            .any(|id| id == child_execution_id)
        {
            doc.execution
                .child_execution_ids
                .push(child_execution_id.to_string());
        }
        doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
        Ok(())
    }

    async fn bind_execution_scope(
        &self,
        execution_id: &str,
        task_id: Option<String>,
        root_execution_id: Option<String>,
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        doc.execution.task_id = task_id;
        doc.execution.root_execution_id = root_execution_id;
        doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
        Ok(())
    }

    async fn update_execution_owner_snapshot(
        &self,
        execution_id: &str,
        active_owner_agent_id: &str,
        owner_stack: &[String],
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        doc.execution.active_owner_agent_id = active_owner_agent_id.to_string();
        doc.execution.owner_stack = owner_stack.to_vec();
        doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
        Ok(())
    }

    async fn replace_active_delegation_group(
        &self,
        execution_id: &str,
        active_delegation_group: &[String],
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        doc.execution.active_delegation_group = active_delegation_group.to_vec();
        doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
        Ok(())
    }

    async fn get_execution_status(
        &self,
        execution_id: &str,
    ) -> Result<magician::magician_v2::storage::WaitingState, V2StorageError> {
        self.executions
            .lock()
            .await
            .get(execution_id)
            .map(|doc| doc.execution.waiting_state.clone())
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))
    }

    async fn create_slot(
        &self,
        _execution_id: &str,
        _name: String,
        _schema_json: serde_json::Value,
        _required: bool,
        _asked_turn_id: Option<String>,
    ) -> Result<magician::magician_v2::storage::V2Slot, V2StorageError> {
        Err(V2StorageError::Storage(
            "Not implemented in mock".to_string(),
        ))
    }

    async fn update_slot_answer(
        &self,
        _execution_id: &str,
        _slot_id: &str,
        _answer: serde_json::Value,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn update_slot_status(
        &self,
        _execution_id: &str,
        _slot_id: &str,
        _status: magician::magician_v2::storage::V2SlotStatus,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn get_slots(
        &self,
        _execution_id: &str,
    ) -> Result<Vec<magician::magician_v2::storage::V2Slot>, V2StorageError> {
        Ok(vec![])
    }

    async fn get_pending_slots(
        &self,
        _execution_id: &str,
    ) -> Result<Vec<magician::magician_v2::storage::V2Slot>, V2StorageError> {
        Ok(vec![])
    }

    async fn get_slot(
        &self,
        _execution_id: &str,
        _slot_id: &str,
    ) -> Result<magician::magician_v2::storage::V2Slot, V2StorageError> {
        Err(V2StorageError::SlotNotFound(_slot_id.to_string()))
    }

    async fn append_state(
        &self,
        execution_id: &str,
        state: StateBundle,
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;

        doc.states.push(state);
        Ok(())
    }

    async fn get_states(&self, execution_id: &str) -> Result<Vec<StateBundle>, V2StorageError> {
        let executions = self.executions.lock().await;
        let doc = executions
            .get(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;

        let mut states = doc.states.clone();
        states.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(states)
    }

    async fn get_latest_state(
        &self,
        execution_id: &str,
    ) -> Result<Option<StateBundle>, V2StorageError> {
        let executions = self.executions.lock().await;
        let doc = executions
            .get(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;

        Ok(doc
            .states
            .iter()
            .cloned()
            .max_by(|a, b| a.created_at.cmp(&b.created_at)))
    }

    async fn store_strategy_attempts(
        &self,
        _execution_id: &str,
        _turn_id: &str,
        attempts: Vec<magician::magician_v2::storage::StrategyAttempt>,
        processing_metadata: ProcessingMetadata,
        recommended_questions: Option<Vec<Self::RecommendedQuestion>>,
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(_execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(_execution_id.to_string()))?;

        let turn = doc
            .turns
            .iter_mut()
            .find(|t| t.id == _turn_id)
            .ok_or_else(|| V2StorageError::TurnNotFound(_turn_id.to_string()))?;

        turn.strategy_attempts = attempts;
        turn.processing_metadata = Some(processing_metadata);
        turn.recommended_questions = recommended_questions;
        Ok(())
    }

    async fn update_processing_correlation_id(
        &self,
        _execution_id: &str,
        _correlation_id: Option<String>,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn update_execution_processing_stage(
        &self,
        _execution_id: &str,
        _stage: Option<String>,
        _provider: Option<String>,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn update_turn_enriched_query(
        &self,
        execution_id: &str,
        turn_id: &str,
        enriched_query: Option<String>,
    ) -> Result<(), V2StorageError> {
        let mut executions = self.executions.lock().await;
        let doc = executions
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;

        let turn = doc
            .turns
            .iter_mut()
            .find(|t| t.id == turn_id)
            .ok_or_else(|| V2StorageError::TurnNotFound(turn_id.to_string()))?;

        turn.enriched_query = enriched_query;
        Ok(())
    }

    async fn load_clarification_session(
        &self,
        execution_id: &str,
    ) -> Result<Option<Self::ClarificationSession>, V2StorageError> {
        Ok(self
            .executions
            .lock()
            .await
            .get(execution_id)
            .and_then(|doc| doc.clarification_session.clone()))
    }

    async fn store_clarification_session(
        &self,
        execution_id: &str,
        session: Self::ClarificationSession,
    ) -> Result<(), V2StorageError> {
        let mut guard = self.executions.lock().await;
        let doc = guard
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        doc.clarification_session = Some(session);
        Ok(())
    }

    async fn delete_clarification_session(&self, execution_id: &str) -> Result<(), V2StorageError> {
        let mut guard = self.executions.lock().await;
        let doc = guard
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        doc.clarification_session = None;
        Ok(())
    }

    async fn get_clarification_history(
        &self,
        execution_id: &str,
    ) -> Result<Vec<Self::ClarificationHistoryEntry>, V2StorageError> {
        let guard = self.executions.lock().await;
        let doc = guard
            .get(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        Ok(doc.clarification_history.clone())
    }

    async fn append_clarification_history(
        &self,
        execution_id: &str,
        entries: Vec<Self::ClarificationHistoryEntry>,
    ) -> Result<(), V2StorageError> {
        let mut guard = self.executions.lock().await;
        let doc = guard
            .get_mut(execution_id)
            .ok_or_else(|| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        doc.clarification_history.extend(entries);
        Ok(())
    }
}

pub struct NoopSessionManager;

#[async_trait::async_trait]
impl SessionManager for NoopSessionManager {
    async fn load(
        &self,
        _workflow_id: &str,
    ) -> Result<
        Option<magician::magician_v2::ask_loop::ClarificationSession>,
        magician::magician_v2::ask_loop::ClarificationSessionStoreError,
    > {
        Ok(None)
    }

    async fn save(
        &self,
        _session: magician::magician_v2::ask_loop::ClarificationSession,
    ) -> Result<(), magician::magician_v2::ask_loop::ClarificationSessionStoreError> {
        Ok(())
    }

    async fn delete(
        &self,
        _workflow_id: &str,
    ) -> Result<(), magician::magician_v2::ask_loop::ClarificationSessionStoreError> {
        Ok(())
    }

    async fn append_question(
        &self,
        _workflow_id: &str,
        _question: magician::magician_v2::ask_loop::ClarifierQuestion,
    ) -> Result<(), magician::magician_v2::ask_loop::ClarificationSessionStoreError> {
        Ok(())
    }

    async fn mark_question_answered(
        &self,
        _workflow_id: &str,
        _question_id: &str,
        _slots: Vec<magician::magician_v2::slot_graph::SlotRecord>,
    ) -> Result<(), magician::magician_v2::ask_loop::ClarificationSessionStoreError> {
        Ok(())
    }

    async fn cancel_questions(
        &self,
        _workflow_id: &str,
        _question_ids: &[String],
    ) -> Result<(), magician::magician_v2::ask_loop::ClarificationSessionStoreError> {
        Ok(())
    }

    async fn mark_state(
        &self,
        _workflow_id: &str,
        _state: magician::magician_v2::ask_loop::ClarificationSessionState,
    ) -> Result<(), magician::magician_v2::ask_loop::ClarificationSessionStoreError> {
        Ok(())
    }

    async fn update_batch_progress(
        &self,
        _workflow_id: &str,
        _batch_id: &str,
        _answered: usize,
        _total: usize,
    ) -> Result<(), magician::magician_v2::ask_loop::ClarificationSessionStoreError> {
        Ok(())
    }

    async fn ensure_round_limit(
        &self,
        _workflow_id: &str,
        _limit: usize,
    ) -> Result<(), magician::magician_v2::ask_loop::ClarificationSessionStoreError> {
        Ok(())
    }

    async fn mark_questions_handed_off(
        &self,
        _workflow_id: &str,
        _question_ids: &[String],
    ) -> Result<usize, magician::magician_v2::ask_loop::ClarificationSessionStoreError> {
        Ok(0)
    }
}

// ============================================================================
// Mock LLM and ToolDiscovery for Testing
// ============================================================================

pub struct MockLLMService;

#[async_trait::async_trait]
impl QueryAnalysisLLM for MockLLMService {
    async fn generate_analysis(&self, _prompt: &str) -> anyhow::Result<SimplifiedLLMResponse> {
        // Return minimal valid JSON matching the actual QueryAnalysisLLM trait
        Ok(SimplifiedLLMResponse::content_only(r#"{
            "complexity": {"score": 0.3, "factors": [], "reasoning": "simple"},
            "categories": {"categories": ["test"], "reasoning": "test"},
            "dependencies": {"is_multi_step": false, "dependencies": [], "workflow_steps": [], "reasoning": "single"},
            "resource_estimate": {"expected_tokens": 100, "expected_duration_ms": 1000, "expected_iterations": 1},
            "extracted_entities": {"entities": {}, "extraction_confidence": 0.9, "extraction_reasoning": "test"}
        }"#.to_string()))
    }
}

pub struct MockPromptStore;

#[async_trait::async_trait]
impl magician::magician_v2::prompts::PromptStore for MockPromptStore {
    async fn get_prompt(
        &self,
        name: &str,
        _version: &str,
    ) -> anyhow::Result<magician::magician_v2::prompts::Prompt> {
        use magician::magician_v2::prompts::{
            Prompt, PromptCategory, PromptMetadata, PromptVariable,
        };
        let variable = |name: &str, required: bool| PromptVariable {
            name: name.to_string(),
            description: format!("{name} variable"),
            required,
            default_value: if required { None } else { Some(String::new()) },
            examples: vec![format!("example {name}")],
        };

        let (prompt_name, content, variables, category) = if name
            == magician::magician_v2::prompts::constants::names::AGENTIC_DECISION
        {
            (
                name.to_string(),
                r#"## GOAL
{goal}

## SUCCESS CRITERIA
{success_criteria}

## CURRENT STATE ({state_type})
{state_description}

## EXECUTION HISTORY
{history_summary}
{hint_section}
{input_context}

## YOUR DECISION
Respond with JSON only."#
                    .to_string(),
                vec![
                    variable("goal", true),
                    variable("success_criteria", true),
                    variable("state_type", true),
                    variable("state_description", true),
                    variable("history_summary", true),
                    variable("hint_section", false),
                    variable("input_context", false),
                ],
                PromptCategory::General,
            )
        } else if name == magician::magician_v2::prompts::constants::names::AGENTIC_DECISION_SYSTEM
        {
            (
                    name.to_string(),
                    "You are an agentic executor. {identity_section}\n{capabilities_section}\nRespond with JSON only.".to_string(),
                    vec![
                        variable("identity_section", true),
                        variable("capabilities_section", true),
                    ],
                    PromptCategory::AgenticExecution,
                )
        } else {
            (
                "test_prompt".to_string(),
                "Test prompt template: {query}".to_string(),
                vec![PromptVariable {
                    name: "query".to_string(),
                    description: "The query text".to_string(),
                    required: false,
                    default_value: Some(String::new()),
                    examples: vec!["test query".to_string()],
                }],
                PromptCategory::QueryAnalysis,
            )
        };

        Ok(Prompt {
            name: prompt_name,
            version: "1.0.0".to_string(),
            content,
            variables,
            metadata: PromptMetadata {
                category,
                description: "Test prompt".to_string(),
                author: "test".to_string(),
                created_at: chrono::Utc::now(),
                changelog: "Initial version".to_string(),
                tags: vec![],
                estimated_tokens: Some(100),
            },
        })
    }

    async fn list_versions(&self, _name: &str) -> anyhow::Result<Vec<String>> {
        Ok(vec!["1.0.0".to_string()])
    }

    async fn list_prompt_names(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec!["test_prompt".to_string()])
    }

    async fn save_prompt(
        &self,
        _prompt: &magician::magician_v2::prompts::Prompt,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn prompt_exists(&self, _name: &str, _version: &str) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn latest_version(&self, _name: &str) -> anyhow::Result<String> {
        Ok("1.0.0".to_string())
    }

    async fn delete_prompt(&self, _name: &str, _version: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn initialize(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

pub struct TestRuntimeConfig {
    storage_path: String,
}

impl RuntimeConfig for TestRuntimeConfig {
    fn realtime_events_enabled(&self) -> bool {
        false
    }

    fn storage_path(&self) -> &str {
        &self.storage_path
    }

    fn max_conversations(&self) -> usize {
        4
    }

    fn conversation_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(30)
    }
}

pub struct MockToolDiscovery;

#[async_trait::async_trait]
impl ToolDiscovery for MockToolDiscovery {
    async fn find_best_match_with_context(
        &self,
        _task: &str,
        _context: &ExecutionContext,
    ) -> ToolMatchResult {
        ToolMatchResult {
            primary_match: None,
            match_confidence: 0.0,
            missing_capabilities: vec![],
            parameter_coverage: 0.0,
            executable: false,
        }
    }

    async fn find_multiple_matches_with_context(
        &self,
        _task: &str,
        _context: &ExecutionContext,
    ) -> MultipleToolMatchResult {
        MultipleToolMatchResult {
            matches: vec![],
            match_strategies: vec![],
            confidence_spread: 0.0,
            recommended_approach: None,
            any_executable: false,
            aggregate_missing_capabilities: vec![],
        }
    }

    async fn is_tool_available(&self, _tool_name: &str, _context: &ExecutionContext) -> bool {
        false
    }

    async fn get_tool_metadata(
        &self,
        _tool_name: &str,
        _context: &ExecutionContext,
    ) -> Option<HashMap<String, serde_json::Value>> {
        None
    }

    async fn get_available_tools(&self, _context: &ExecutionContext) -> Vec<String> {
        vec![]
    }

    async fn get_available_categories(&self, _context: &ExecutionContext) -> Vec<String> {
        vec!["test".to_string()]
    }
}

/// The catalog the harness serves, over the mock discovery.
///
/// Everything delegates, except the one question an agent definition asks:
/// **what do my declared tools resolve to?** A deployment answers it from the
/// compiled packs (`embedded_compiled_pack_defs` — what the binary falls back
/// to when no skills dir provides them), so an agent that declares `http`
/// resolves the same umbrella tool a real one does and the dispatch ceiling
/// admits the universal packs that ride on a non-empty grant. An agent that
/// declares nothing still resolves nothing, which is what every fixture built
/// before this seam expects: `agent_filtered_tools`' default would read an
/// empty allowlist as "no restriction" and hand such an agent the whole
/// catalog, which is a different agent than the one the test wrote.
pub struct HarnessToolCatalog {
    inner: Arc<magician::magician_v2::tooling::ToolDiscoveryAdapter>,
}

#[async_trait::async_trait]
impl ToolCatalog for HarnessToolCatalog {
    async fn list_tool_names(&self, context: &ExecutionContext) -> Vec<String> {
        self.inner.list_tool_names(context).await
    }

    async fn available_categories(&self, context: &ExecutionContext) -> Vec<String> {
        self.inner.available_categories(context).await
    }

    async fn get_tool_metadata(
        &self,
        tool_name: &str,
        context: &ExecutionContext,
    ) -> Option<HashMap<String, serde_json::Value>> {
        self.inner.get_tool_metadata(tool_name, context).await
    }

    async fn filtered_tools_by_categories(
        &self,
        categories: &[String],
        context: &ExecutionContext,
    ) -> anyhow::Result<Vec<runtime_core::ToolInfo>> {
        self.inner
            .filtered_tools_by_categories(categories, context)
            .await
    }

    async fn all_tools(
        &self,
        context: &ExecutionContext,
    ) -> anyhow::Result<Vec<runtime_core::ToolInfo>> {
        self.inner.all_tools(context).await
    }

    async fn agent_filtered_tools(
        &self,
        tools: &[String],
        excluded_tools: &[String],
        _context: &ExecutionContext,
    ) -> anyhow::Result<Vec<runtime_core::ToolInfo>> {
        if tools.is_empty() {
            return Ok(Vec::new());
        }
        Ok(embedded_pack_tools()
            .into_iter()
            .filter(|tool| tools.iter().any(|declared| declared == &tool.name))
            .filter(|tool| !excluded_tools.iter().any(|denied| denied == &tool.name))
            .collect())
    }

    async fn category_tool_counts(
        &self,
        context: &ExecutionContext,
    ) -> anyhow::Result<HashMap<String, usize>> {
        self.inner.category_tool_counts(context).await
    }
}

fn embedded_pack_tools() -> Vec<runtime_core::ToolInfo> {
    magician::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs_ref()
        .iter()
        .map(|pack| runtime_core::ToolInfo {
            name: pack.name.clone(),
            description: pack
                .description
                .clone()
                .unwrap_or_else(|| format!("Capability pack `{}`.", pack.name)),
            category: pack.name.clone(),
            categories: Vec::new(),
            parameters: Vec::new(),
            enhanced_description: None,
            keywords: Vec::new(),
            use_cases: Vec::new(),
            composition_category: None,
            providing_agent_id: None,
        })
        .collect()
}

// ============================================================================
// Test Helpers
// ============================================================================

pub fn unique_integration_storage_path() -> String {
    std::env::temp_dir()
        .join(format!(
            "magician-v2-api-it-{}",
            uuid::Uuid::new_v4().simple()
        ))
        .to_string_lossy()
        .to_string()
}

pub fn integration_shell_native_response() -> ExecutionNativeResponse {
    let mut response = ExecutionNativeResponse::from_tool_calls(vec![ExecutionToolCall {
        id: "integration-shell-tool-call".to_string(),
        name: "bash".to_string(),
        arguments: json!({
            "thinking": "Integration test shell action.",
            "command": "echo magician-p3-14-integration",
            "task_state_action": {
                "action": "none",
                "reason": "Integration test action does not mutate task state."
            }
        }),
    }]);
    response.finish_reason = Some("tool_calls".to_string());
    response
}

pub async fn create_test_v2_api_with_storage_path(
    storage_path: String,
) -> web::Data<MagicianV2Api> {
    std::env::set_var("MAGICIAN_DISABLE_SYSTEM_PROXY", "1");
    let store = Arc::new(MockV2Store::new()) as Arc<dyn V2ConversationStore>;
    let llm_service = Arc::new(MockLLMService) as Arc<dyn QueryAnalysisLLM>;
    let tool_discovery = Arc::new(MockToolDiscovery) as Arc<dyn ToolDiscovery>;

    let prompt_storage =
        Arc::new(MockPromptStore) as Arc<dyn magician::magician_v2::prompts::PromptStore>;
    let prompt_manager = Arc::new(PromptManager::new(Arc::clone(&prompt_storage)));

    let tool_adapter = Arc::new(magician::magician_v2::tooling::ToolDiscoveryAdapter::new(
        tool_discovery,
    ));
    let tool_catalog: Arc<dyn ToolCatalog> = Arc::new(HarnessToolCatalog {
        inner: tool_adapter.clone(),
    });
    let tool_matching: Arc<dyn ToolMatching> = tool_adapter.clone();
    let runtime_config: Arc<dyn RuntimeConfig> = Arc::new(TestRuntimeConfig {
        storage_path: storage_path.clone(),
    });
    let session_manager: Arc<dyn SessionManager> = Arc::new(NoopSessionManager);
    let magicutor_client = Arc::new(
        MagicutorClient::new(MagicutorExecutionConfig::default())
            .expect("failed to create Magicutor client for tests"),
    );
    // The embedded compiled packs (`http`, the internal capabilities) — the
    // set the binary falls back to when no skills dir provides them — so a
    // scripted run can dispatch the same `http` adapter production runs.
    let capability_pack_defs =
        magician::magician_v2::execution::compiled_providers::embedded_compiled_pack_defs();
    let services = MagicianV2Services::new(
        runtime_config,
        Arc::clone(&store),
        Arc::clone(&prompt_manager),
        tool_catalog,
        tool_matching,
        None,
        session_manager,
        magicutor_client,
        capability_pack_defs,
        std::path::PathBuf::from("."),
        None,
    );

    // A realtime broadcaster, so a test can watch the canonical lifecycle
    // (`hitl.requested`, `hitl.resolved`, …) the way the UI and the bots do:
    // `api.orchestrator().event_broadcaster()`.
    let broadcaster =
        Arc::new(magician::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(1024));
    let orchestrator = MagicianV2Orchestrator::new_with_secret_runtime(
        services,
        llm_service,
        magician::magician_v2::secrets::in_memory_secret_runtime_bootstrap(),
    )
    .with_operation_llm_router(Arc::new(OperationLlmRouter::new(None)))
    .with_event_broadcaster(Arc::clone(&broadcaster))
    .initialize();
    #[cfg(debug_assertions)]
    orchestrator.set_test_native_responses(vec![integration_shell_native_response()]);
    let artifact_v2_service = Arc::new(
        magician::magician_v2::artifact_v2::service::ArtifactV2Service::with_orchestrator(
            PathBuf::from(&storage_path).join("magician_data_v3"),
            Arc::clone(&orchestrator),
            magician::magician_v2::gaui::MuijStorage::new(
                PathBuf::from(&storage_path).join("muij"),
            ),
        ),
    );
    orchestrator.set_artifact_v2_service(Arc::clone(&artifact_v2_service));
    // As at boot: the broadcaster projects the run's canonical facts through
    // the artifact service's durable sink, so a journalled `hitl.requested`
    // reaches the feed instead of stalling the outbox projector.
    artifact_v2_service.set_event_broadcaster(broadcaster);
    let pause_states_path = orchestrator.pause_states_storage_path();
    // A harness-built service is a cutover-complete deployment, which is the
    // ordinary state a test means when it asks for a working API: the
    // stateless driver — the default, and the only one selected implicitly —
    // refuses a scope whose legacy-writer cutover is unsealed, and every
    // execution this fixture starts would answer `503
    // stateless_scope_not_activated`. Sealed here rather than per test, for
    // the scopes `test_support` lists as the suite's own.
    magician::magician_v2::test_support::activate_conventional_test_scopes_sync(&pause_states_path);
    let agent_storage_root = resolve_integration_agent_runtime_root(&pause_states_path);
    let agent_storage = AgentStorage::new(agent_storage_root);
    seed_integration_trust_policy_files(&agent_storage);
    let api = web::Data::new(MagicianV2Api::new(orchestrator));
    api.agent_api()
        .runtime
        .set_artifact_v2_service(artifact_v2_service);
    api
}

pub async fn create_test_v2_api() -> web::Data<MagicianV2Api> {
    create_test_v2_api_with_storage_path(unique_integration_storage_path()).await
}

pub fn integration_workspace_layout(
    pause_states_path: &Path,
) -> magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace {
    magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(pause_states_path)
}

pub fn resolve_integration_agent_runtime_root(pause_states_path: &Path) -> PathBuf {
    integration_workspace_layout(pause_states_path)
        .scoped_agent_runtime_root(INTEGRATION_PRINCIPAL, INTEGRATION_WORKSPACE)
}

pub fn resolve_integration_memory_service(pause_states_path: &Path) -> AgentMemoryService {
    let workspace = integration_workspace_layout(pause_states_path);
    AgentMemoryService::with_scoped_memory_scope(
        workspace.memory_root(INTEGRATION_PRINCIPAL, INTEGRATION_WORKSPACE),
        INTEGRATION_PRINCIPAL,
        INTEGRATION_WORKSPACE,
    )
}

pub fn seed_integration_trust_policy_files(storage: &AgentStorage) {
    std::fs::create_dir_all(storage.system_root())
        .expect("integration test should create system root");
    std::fs::write(
        storage.trust_policies_template_path(),
        magician::magician_v2::agents::TrustPolicyFile::recommended_template_yaml(),
    )
    .expect("integration test should write trust policy template");
    let defaults_yaml = serde_yaml::to_string(
        &magician::magician_v2::agents::TrustPolicyFile::recommended_defaults(),
    )
    .expect("integration trust policy defaults should serialize");
    std::fs::write(storage.trust_policies_path(), defaults_yaml)
        .expect("integration test should write trust policy");
}

pub fn deny_integration_bash_for_local_agents(storage: &AgentStorage) {
    let mut policies = TrustPolicyFile::recommended_defaults();
    let local = policies
        .trust_policies
        .iter_mut()
        .find(|policy| policy.level == TrustLevel::LOCAL)
        .expect("recommended trust policies should include local");
    local.deny.push(ToolActionPattern {
        tool: "bash".to_string(),
        action: ActionPattern::Single("*".to_string()),
    });
    let yaml = serde_yaml::to_string(&policies)
        .expect("integration trust policy override should serialize");
    std::fs::write(storage.trust_policies_path(), yaml)
        .expect("integration test should write local bash denial");
}

pub async fn wait_for_episode_for_trigger(
    memory_service: &AgentMemoryService,
    agent_id: &str,
    goal_id: &str,
    trigger_seq: u64,
    timeout: Duration,
) -> V3EpisodeRecord {
    let deadline = Instant::now() + timeout;
    loop {
        let has_episode = memory_service
            .has_episode_for_trigger(agent_id, goal_id, trigger_seq)
            .await
            .expect("episode lookup should succeed");
        if has_episode {
            let episodes = memory_service
                .load_native_episodes(agent_id)
                .await
                .expect("episodes should load");
            if let Some(episode) = episodes
                .into_iter()
                .find(|episode| episode.goal_id() == goal_id && episode.trigger_seq == trigger_seq)
            {
                return episode;
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for episode (agent_id={agent_id}, goal_id={goal_id}, trigger_seq={trigger_seq})"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Run the terminal-outbox projector the way the binary does at boot, at a
/// test cadence: a run that parks at a pause (or ends) leaves its last
/// journalled events — `hitl.requested` among them — as terminal outbox
/// debt that only this lifecycle owner delivers to the feed. Returns the
/// token that stops it.
pub fn spawn_terminal_outbox_projector(
    api: &web::Data<MagicianV2Api>,
) -> tokio_util::sync::CancellationToken {
    use magician::magician_v2::execution::agentic::run_loop::{
        state::WorkerId,
        store::fs::FsLoopStateStore,
        terminal_outbox::{project_terminal_outbox_debt_registered, TerminalProjectionJobRegistry},
    };
    let artifact_service = api
        .orchestrator()
        .artifact_v2_service()
        .expect("the harness installs an artifact service");
    // The loop store lives under the orchestrator's runtime root (where the
    // driver journals), which this harness keeps beside — not under — the
    // artifact workspace; the binary's two roots coincide.
    let store = FsLoopStateStore::new(api.orchestrator().pause_states_storage_path());
    let broadcaster = api
        .orchestrator()
        .event_broadcaster()
        .expect("the harness installs a broadcaster");
    let shutdown = tokio_util::sync::CancellationToken::new();
    let cancel = shutdown.clone();
    tokio::spawn(async move {
        let jobs = TerminalProjectionJobRegistry::new();
        let worker = WorkerId::new(format!(
            "test-terminal-outbox-projector-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let max_visits = std::num::NonZeroUsize::new(128).expect("non-zero");
        let mut resume = None;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(200)) => {},
            }
            match project_terminal_outbox_debt_registered(
                &store,
                &broadcaster,
                Some(&artifact_service),
                &worker,
                resume.as_ref(),
                max_visits,
                &shutdown,
                &jobs,
            )
            .await
            {
                Ok(report) => {
                    if report.discovered > 0
                        || report.store_failures > 0
                        || report.canonical_persistence_failures > 0
                    {
                        tracing::warn!(?report, "[TEST-HARNESS] terminal outbox projection pass");
                    }
                    resume = report.resume;
                },
                Err(error) => {
                    tracing::warn!(%error, "[TEST-HARNESS] terminal outbox projection failed");
                    resume = None;
                },
            }
        }
    });
    cancel
}
