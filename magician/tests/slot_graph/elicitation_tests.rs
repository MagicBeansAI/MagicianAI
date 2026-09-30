use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use magician::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    ask_loop::clarifier::{ClarifierError, ClarifierLibrary, ClarifierQuestion},
    confidence::{ConfidenceConfig, ConfidenceService},
    prompts::{json_storage::JsonStorageConfig, JsonPromptStorage, PromptManager},
    slot_graph::elicitation::{
        STAGE_ELICITATION_OUTCOME, STAGE_ELICITATION_REWRITE, STAGE_ELICITATION_SLOTS,
        STAGE_SLOT_PERSISTENCE,
    },
    slot_graph::{
        ElicitationConfig, ElicitationService, EnrichmentPipeline, ExtractionConfig,
        LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService, QuestionRewriter,
        RewriteModel, RewriteModelRequest, RewriteModelResponse, RewriteStrategy, RewriterConfig,
        SlotExtractor, SlotGraphRepository, SlotRecord, SlotType,
    },
    state_tracker::{StageContext, StateTracker},
    storage::file::FileV2Store,
};
use tempfile::tempdir;
use tokio::sync::RwLock;

struct StubFunctionLlm {
    responses: Mutex<Vec<String>>,
}

impl StubFunctionLlm {
    fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(String::from).collect()),
        }
    }
}

#[async_trait]
impl LlmService for StubFunctionLlm {
    async fn call_function(
        &self,
        _request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        let mut guard = self.responses.lock().expect("stub guard poisoned");
        if guard.is_empty() {
            Err(anyhow!("no more responses"))
        } else {
            Ok(LlmFunctionCallResponse {
                raw_arguments: guard.remove(0),
                telemetry: None,
            })
        }
    }
}

struct StubRewriteModel {
    responses: Mutex<Vec<(RewriteStrategy, String)>>,
}

impl StubRewriteModel {
    fn new(mapping: Vec<(RewriteStrategy, &str)>) -> Self {
        let responses = mapping
            .into_iter()
            .map(|(strategy, body)| (strategy, body.to_string()))
            .collect();
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl RewriteModel for StubRewriteModel {
    async fn generate(&self, request: RewriteModelRequest) -> Result<RewriteModelResponse> {
        let mut guard = self.responses.lock().expect("stub guard poisoned");
        if let Some(index) = guard
            .iter()
            .position(|(strategy, _)| *strategy == request.strategy)
        {
            let (_, body) = guard.remove(index);
            Ok(RewriteModelResponse {
                completion: body,
                telemetry: None,
            })
        } else {
            Err(anyhow!(
                "missing response for strategy {:?}",
                request.strategy
            ))
        }
    }
}

struct RecordingSlotRepository {
    stored: RwLock<Vec<SlotRecord>>,
}

impl RecordingSlotRepository {
    fn new() -> Self {
        Self {
            stored: RwLock::new(Vec::new()),
        }
    }

    async fn all(&self) -> Vec<SlotRecord> {
        self.stored.read().await.clone()
    }
}

#[async_trait]
impl SlotGraphRepository for RecordingSlotRepository {
    async fn create_slot(&self, slot: &SlotRecord) -> Result<()> {
        self.stored.write().await.push(slot.clone());
        Ok(())
    }
}

struct EchoClarifierLlm;

#[async_trait]
impl magician::magician_v2::ask_loop::clarifier::LlmService for EchoClarifierLlm {
    async fn generate(&self, prompt: &str) -> Result<String, ClarifierError> {
        Ok(prompt.to_string())
    }
}

async fn prompt_manager() -> Arc<PromptManager> {
    let storage =
        JsonPromptStorage::new(JsonStorageConfig::default()).expect("prompt storage initialised");
    Arc::new(PromptManager::new(Arc::new(storage)))
}

fn extractor(responses: Vec<&str>, manager: Arc<PromptManager>) -> Arc<SlotExtractor> {
    Arc::new(SlotExtractor::new(
        Arc::new(StubFunctionLlm::new(responses)),
        manager,
        ExtractionConfig::default(),
    ))
}

fn rewriter(
    mapping: Vec<(RewriteStrategy, &str)>,
    manager: Arc<PromptManager>,
) -> Arc<QuestionRewriter> {
    Arc::new(QuestionRewriter::new(
        Arc::new(StubRewriteModel::new(mapping)),
        manager,
        RewriterConfig::default(),
    ))
}

fn confidence_service() -> Arc<ConfidenceService> {
    let mut config = ConfidenceConfig::default();
    config.unresolved_slot_threshold = 0.65;
    Arc::new(ConfidenceService::new(config))
}

#[tokio::test]
async fn returns_clarified_task_when_confident() {
    let manager = prompt_manager().await;
    let extractor = extractor(
        vec![
            r#"{"slots": [{
            "slot_type": "entity",
            "value": {"name": "Launch Plan", "type": "project"},
            "confidence": 0.92,
            "rationale": "User explicitly asked to plan a launch"
        }], "metadata": {"attempt": 1}}"#,
        ],
        manager.clone(),
    );

    let rewrite_mapping = vec![(
        RewriteStrategy::Aggressive,
        r#"{
            "clarified_task": "Deliver a complete launch plan for the new feature.",
            "constraints": ["Include risk assessment"],
            "objectives": ["Outline timeline"],
            "resources": [],
            "open_questions": [],
            "confidence": 0.84
        }"#,
    )];

    let rewriter = rewriter(rewrite_mapping, manager.clone());
    let enrichment = Arc::new(EnrichmentPipeline::default());
    let confidence = confidence_service();
    let repository = Arc::new(RecordingSlotRepository::new());

    let service = ElicitationService::new(
        extractor,
        enrichment,
        rewriter,
        confidence,
        repository.clone(),
        None,
        None,
        None, // RuntimeTransportBroadcaster
        ElicitationConfig::default(),
    );

    let result = service
        .elicit_and_rewrite("wf-123", "Plan the beta launch", None, None, None)
        .await
        .expect("elicitation succeeds");

    assert!(
        !result.needs_clarification,
        "high confidence should not request clarification"
    );
    assert!(
        result.recommended_questions.is_empty(),
        "no questions expected for confident flow"
    );
    assert!(
        !result.clarified_task.clarified_task.is_empty(),
        "clarified task should contain planner-ready text"
    );

    let stored = repository.all().await;
    assert_eq!(stored.len(), 1, "slot should be persisted");
    assert!(
        stored[0].id.starts_with("wf-123::"),
        "slot id is namespaced to workflow"
    );
}

#[tokio::test]
async fn open_questions_trigger_clarification_flow() {
    let manager = prompt_manager().await;
    let extractor = extractor(
        vec![
            r#"{"slots": [{
            "slot_type": "entity",
            "value": {"name": "Deploy Service", "type": "task"},
            "confidence": 0.92,
            "rationale": "User asked to deploy a service"
        }], "metadata": {"attempt": 1}}"#,
        ],
        manager.clone(),
    );

    let rewriter = rewriter(
        vec![(
            RewriteStrategy::Aggressive,
            r#"{
                "clarified_task": "Prepare deployment plan",
                "constraints": [],
                "objectives": [],
                "resources": [],
                "confidence": 0.8,
                "open_questions": ["Which service should I deploy?"]
            }"#,
        )],
        manager.clone(),
    );

    let enrichment = Arc::new(EnrichmentPipeline::default());
    let confidence = confidence_service();
    let repository = Arc::new(RecordingSlotRepository::new());

    let service = ElicitationService::new(
        extractor,
        enrichment,
        rewriter,
        confidence,
        repository.clone(),
        None,
        None,
        None,
        ElicitationConfig::default(),
    );

    let result = service
        .elicit_and_rewrite("wf-open", "Deploy the service", None, None, None)
        .await
        .expect("elicitation succeeds");

    assert!(
        result.needs_clarification,
        "open questions from rewriter should force clarification"
    );
    assert_eq!(
        result
            .clarified_task
            .open_questions
            .iter()
            .map(|q| q.question_text.clone())
            .collect::<Vec<_>>(),
        vec!["Which service should I deploy?".to_string()],
        "clarified task should retain open questions"
    );
    assert!(
        result
            .recommended_questions
            .iter()
            .any(|q| q.question_text == "Which service should I deploy?"),
        "recommended questions should mirror the rewriter open question"
    );
}

#[tokio::test]
async fn emits_clarifier_questions_when_confidence_low() {
    let manager = prompt_manager().await;
    let extractor = extractor(
        vec![
            r#"{"slots": [{
            "slot_type": "status",
            "value": {"value": "unknown"},
            "confidence": 0.3,
            "rationale": "Confidence intentionally low for testing"
        }]} "#,
        ],
        manager.clone(),
    );

    let rewriter = rewriter(
        vec![(
            RewriteStrategy::Conservative,
            r#"{
                "clarified_task": "Fallback task",
                "constraints": [],
                "objectives": [],
                "resources": [],
                "open_questions": [],
                "confidence": 0.5
            }"#,
        )],
        manager.clone(),
    );

    let enrichment = Arc::new(EnrichmentPipeline::default());
    let confidence = confidence_service();
    let repository = Arc::new(RecordingSlotRepository::new());
    let clarifier = ClarifierLibrary::with_default_templates(Arc::new(EchoClarifierLlm));

    let service = ElicitationService::new(
        extractor,
        enrichment,
        rewriter,
        confidence,
        repository.clone(),
        Some(Arc::new(clarifier)),
        None,
        None, // RuntimeTransportBroadcaster
        ElicitationConfig {
            confidence_gate: 0.7,
            max_questions: 2,
        },
    );

    let result = service
        .elicit_and_rewrite("wf-low", "Check server status", None, None, None)
        .await
        .expect("elicitation succeeds");

    assert!(
        result.needs_clarification,
        "low confidence slot should trigger clarification pathway"
    );
    assert!(
        !result.recommended_questions.is_empty(),
        "at least one clarifier question should be returned"
    );
    assert!(
        !result.clarified_task.clarified_task.is_empty(),
        "clarified task should be preserved even when clarification is required"
    );

    fn is_valid_question(question: &ClarifierQuestion) -> bool {
        !question.question_text.is_empty()
    }

    assert!(
        result.recommended_questions.iter().any(is_valid_question),
        "clarifier questions should include text"
    );

    let stored = repository.all().await;
    assert_eq!(stored.len(), 1, "slot should still be persisted");
    assert_eq!(stored[0].slot_type, SlotType::Status);
}

#[tokio::test]
async fn resume_skips_failed_rewrite_without_duplicate_slots() {
    let workflow_id = "wf-resume";
    let task_id = format!("task-{workflow_id}");
    let manager = prompt_manager().await;
    let extractor = extractor(
        vec![
            r#"{"slots": [{
                "slot_type": "entity",
                "value": {"name": "Launch Plan", "type": "project"},
                "confidence": 0.9,
                "rationale": "User asked for a launch plan"
            }]}"#,
            // Second response for resume attempt
            r#"{"slots": [{
                "slot_type": "entity",
                "value": {"name": "Launch Plan", "type": "project"},
                "confidence": 0.9,
                "rationale": "User asked for a launch plan"
            }]}"#,
        ],
        manager.clone(),
    );

    let enrichment = Arc::new(EnrichmentPipeline::default());
    let confidence = confidence_service();
    let repository = Arc::new(RecordingSlotRepository::new());

    // Isolate state per test run; avoid reusing workflow_id in shared tmp dir.
    let temp_dir = tempdir().expect("tempdir");
    let artifact_workspace =
        ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(temp_dir.path()));
    artifact_workspace
        .ensure_task_workspace(workflow_id, "test-workspace", &task_id)
        .await
        .expect("create task workspace for state tracking");
    let file_store = Arc::new(FileV2Store::with_workspace_layout(artifact_workspace));
    let state_tracker = Arc::new(StateTracker::with_confidence_service(
        file_store,
        Arc::clone(&confidence),
    ));

    let failing_rewriter = Arc::new(QuestionRewriter::new(
        Arc::new(StubRewriteModel::new(Vec::new())),
        manager.clone(),
        RewriterConfig::default(),
    ));

    let service = ElicitationService::new(
        Arc::clone(&extractor),
        Arc::clone(&enrichment),
        failing_rewriter,
        Arc::clone(&confidence),
        Arc::clone(&repository) as Arc<dyn SlotGraphRepository>,
        None,
        Some(Arc::clone(&state_tracker)),
        None, // RuntimeTransportBroadcaster
        ElicitationConfig::default(),
    );

    // Precreate the workflow execution so state tracking has a backing document.
    state_tracker
        .conversation_store()
        .create_execution_with_options(
            workflow_id,
            "test-workspace",
            None,
            magician::magician_v2::chat::DEFAULT_AGENT_ID,
            Some(task_id),
            Some(workflow_id.to_string()),
            Some(workflow_id.to_string()),
            None,
            None,
            Vec::new(),
            magician::magician_v2::storage::WaitingState::Planning,
        )
        .await
        .expect("create execution for state tracking");
    let user_message = "Plan the beta launch";

    let first_attempt = service
        .elicit_and_rewrite(
            workflow_id,
            user_message,
            None,
            None,
            Some(StageContext::PlanningBootstrap),
        )
        .await;
    assert!(first_attempt.is_err(), "initial rewrite should fail");

    let persisted_before = repository.all().await;
    assert_eq!(persisted_before.len(), 1, "slot persisted before failure");

    if let Ok(Some(latest)) = state_tracker.latest_state(workflow_id).await {
        assert_eq!(
            latest
                .failed_stage
                .as_ref()
                .map(|info| info.stage_name.as_str()),
            Some(STAGE_ELICITATION_REWRITE),
            "failure should record rewrite stage"
        );
        assert!(
            latest
                .completed_stages
                .iter()
                .any(|cp| cp.stage_name == STAGE_ELICITATION_SLOTS),
            "extracted slots checkpoint persisted"
        );
        assert!(
            latest
                .completed_stages
                .iter()
                .any(|cp| cp.stage_name == STAGE_SLOT_PERSISTENCE),
            "slot persistence checkpoint persisted"
        );
    }

    let success_rewriter = Arc::new(QuestionRewriter::new(
        Arc::new(StubRewriteModel::new(vec![
            (
                RewriteStrategy::Conservative,
                r#"{
                    "clarified_task": "Deliver a comprehensive launch plan",
                    "constraints": [],
                    "objectives": ["Timeline", "Risk assessment"],
                    "resources": [],
                    "confidence": 0.86,
                    "open_questions": [],
                    "slot_graph_id": "resume-slot-graph"
                }"#,
            ),
            (
                RewriteStrategy::Aggressive,
                r#"{
                    "clarified_task": "Deliver a comprehensive launch plan",
                    "constraints": [],
                    "objectives": ["Timeline", "Risk assessment"],
                    "resources": [],
                    "confidence": 0.86,
                    "open_questions": [],
                    "slot_graph_id": "resume-slot-graph"
                }"#,
            ),
        ])),
        manager.clone(),
        RewriterConfig::default(),
    ));
    service.set_rewriter(success_rewriter);

    let second_attempt = service
        .elicit_and_rewrite(
            workflow_id,
            user_message,
            None,
            None,
            Some(StageContext::PlanningBootstrap),
        )
        .await
        .expect("resume should succeed");

    assert!(
        !second_attempt.clarified_task.clarified_task.is_empty(),
        "resume should produce clarified task"
    );

    let persisted_after = repository.all().await;
    assert!(
        !persisted_after.is_empty(),
        "slot persistence should have at least one record"
    );

    if let Ok(Some(latest_after)) = state_tracker.latest_state(workflow_id).await {
        assert!(latest_after.failed_stage.is_none(), "failure flag cleared");
        assert!(
            latest_after
                .completed_stages
                .iter()
                .any(|cp| cp.stage_name == STAGE_ELICITATION_OUTCOME),
            "elicitation outcome checkpoint recorded"
        );
    }
}
