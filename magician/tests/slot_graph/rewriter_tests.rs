use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::Utc;
use magician::magician_v2::{
    prompts::{json_storage::JsonStorageConfig, JsonPromptStorage, PromptManager},
    slot_graph::{
        ClarifiedTask, QuestionRewriter, RewriteModel, RewriteModelRequest, RewriteModelResponse,
        RewriteStrategy, RewriterConfig, SlotRecord, SlotType,
    },
};
use serde_json::json;

struct TestRewriteModel {
    responses: Mutex<HashMap<RewriteStrategy, String>>,
}

impl TestRewriteModel {
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
impl RewriteModel for TestRewriteModel {
    async fn generate(&self, request: RewriteModelRequest) -> Result<RewriteModelResponse> {
        let mut guard = self.responses.lock().expect("mutex poisoned");
        guard
            .remove(&request.strategy)
            .map(|body| RewriteModelResponse {
                completion: body,
                telemetry: None,
            })
            .ok_or_else(|| anyhow!("missing response for {:?}", request.strategy))
    }
}

async fn make_prompt_manager() -> Arc<PromptManager> {
    let storage = JsonPromptStorage::new(JsonStorageConfig::default())
        .expect("json prompt storage constructed");
    Arc::new(PromptManager::new(Arc::new(storage)))
}

fn make_slot(
    id: &str,
    slot_type: SlotType,
    value: serde_json::Value,
    confidence: f64,
) -> SlotRecord {
    SlotRecord {
        id: id.to_string(),
        slot_type,
        value,
        confidence,
        provenance: vec![],
        evidence_links: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn assert_valid_task(task: &ClarifiedTask) {
    assert!(
        !task.clarified_task.is_empty(),
        "clarified task should not be empty"
    );
    assert_eq!(
        task.slot_graph_id.len(),
        64,
        "slot graph id should be blake3 hex"
    );
}

#[tokio::test]
async fn prefers_higher_scoring_aggressive_candidate() {
    let llm = Arc::new(TestRewriteModel::new(vec![
        (
            RewriteStrategy::Conservative,
            r#"{
                "clarified_task": "Plan beta launch with current information.",
                "constraints": ["Must use existing prod data"],
                "objectives": ["Share update with stakeholders"],
                "resources": [],
                "open_questions": [],
                "confidence": 0.58
            }"#,
        ),
        (
            RewriteStrategy::Aggressive,
            r#"{
                "clarified_task": "Deliver a beta launch plan for Acme CRM covering EU rollout.",
                "constraints": ["Keep launch budget under $25k", "Include customer success sign-off"],
                "objectives": ["Map timeline by Friday", "Highlight risk mitigations"],
                "resources": ["internal_launch_playbook.pdf"],
                "open_questions": [],
                "confidence": 0.72
            }"#,
        ),
    ]));

    let prompt_manager = make_prompt_manager().await;
    let rewriter = QuestionRewriter::new(llm, prompt_manager, RewriterConfig::default());

    let slot_graph = vec![
        make_slot(
            "customer",
            SlotType::Entity,
            json!({"name": "Acme Corp"}),
            0.81,
        ),
        make_slot(
            "launch_deadline",
            SlotType::Temporal,
            json!({"date": "Friday"}),
            0.74,
        ),
    ];

    let clarified = rewriter
        .rewrite_for_planner(
            "Can you finalize the beta launch plan with latest metrics?",
            "Can you finalize the beta launch plan with latest metrics?",
            &slot_graph,
            &[String::from("budget_range")],
        )
        .await
        .expect("rewrite succeeds");

    assert_valid_task(&clarified);
    assert!(
        clarified.constraints.iter().any(|c| c.contains("budget")),
        "constraints should include an inferred budget guardrail"
    );
    assert!(
        clarified
            .open_questions
            .iter()
            .any(|q| q.question_text.contains("budget_range")),
        "unresolved slot should appear as open question"
    );
    assert!(
        clarified.confidence >= 0.7,
        "confidence should reflect aggressive candidate"
    );
    assert_eq!(
        clarified.clarified_task,
        "Deliver a beta launch plan for Acme CRM covering EU rollout."
    );
}

#[tokio::test]
async fn falls_back_to_conservative_on_aggressive_failure() {
    let llm = Arc::new(TestRewriteModel::new(vec![
        (
            RewriteStrategy::Conservative,
            r#"{
                "clarified_task": "Document the handoff for the on-call team.",
                "constraints": ["Checklist must be complete"],
                "objectives": ["Keep downtime minimal"],
                "resources": [],
                "open_questions": [],
                "confidence": 0.42
            }"#,
        ),
        (RewriteStrategy::Aggressive, "not-json :: failure"),
    ]));

    let prompt_manager = make_prompt_manager().await;
    let rewriter = QuestionRewriter::new(llm, prompt_manager, RewriterConfig::default());

    let slot_graph = vec![make_slot(
        "service_name",
        SlotType::Entity,
        json!({"name": "Analytics API"}),
        0.9,
    )];

    let unresolved = vec![String::from("pager_rotation")];

    let clarified = rewriter
        .rewrite_for_planner(
            "Prepare tonight's on-call handoff",
            "Prepare tonight's on-call handoff",
            &slot_graph,
            &unresolved,
        )
        .await
        .expect("rewrite succeeds with conservative fallback");

    assert_valid_task(&clarified);
    assert_eq!(
        clarified.confidence,
        RewriterConfig::default().conservative_confidence_floor
    );
    assert!(
        clarified
            .open_questions
            .iter()
            .any(|q| q.question_text.contains("pager_rotation")),
        "fallback should ensure unresolved slot is surfaced"
    );
}
