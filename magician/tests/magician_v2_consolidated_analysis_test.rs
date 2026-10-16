use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use magician::magician_v2::{
    prompts::{json_storage::JsonStorageConfig, JsonPromptStorage, PromptManager},
    query_analysis::operation_llm_router::{QueryAnalysisLLM, SimplifiedLLMResponse},
    storage::{TurnDirection, V2Slot, V2SlotStatus, V2Turn, WaitingState},
    ConversationContext, QueryIntent, UnifiedQueryAnalyzer,
};
/// Test suite for consolidated LLM analysis (intent + slot matching + query
/// analysis)
///
/// This test verifies that the single LLM call consolidation is working
/// correctly.
use runtime_core::{ExecutionContext, MultipleToolMatchResult, ToolDiscovery, ToolMatchResult};

/// Mock LLM service for testing
#[derive(Clone)]
struct MockLLM {
    response: String,
}

impl MockLLM {
    fn new_with_intent_detection() -> Self {
        // Mock response that includes intent detection
        Self {
            response: r#"{
                "intent": "answer_elicitation",
                "slot_match": {
                    "slot_id": "slot_region",
                    "extracted_value": "us-west-2",
                    "match_confidence": 0.95,
                    "reasoning": "The message 'us-west-2' clearly indicates an AWS region",
                    "alternatives": []
                },
                "complexity": {
                    "score": 0.2,
                    "factors": ["simple_response"],
                    "reasoning": "Simple slot answer"
                },
                "categories": {
                    "categories": ["cloud"],
                    "reasoning": "AWS region selection"
                },
                "dependencies": {
                    "is_multi_step": false,
                    "dependencies": [],
                    "workflow_steps": [],
                    "reasoning": "Single value response"
                },
                "extracted_entities": {
                    "entities": {"region": "us-west-2"},
                    "typed_entities": {"region": ["us-west-2"]},
                    "extraction_confidence": 0.95,
                    "extraction_reasoning": "Extracted AWS region"
                }
            }"#
            .to_string(),
        }
    }

    fn new_with_new_task() -> Self {
        // Mock response for new task
        Self {
            response: r#"{
                "intent": "new_task",
                "complexity": {
                    "score": 0.3,
                    "factors": ["network_operation"],
                    "reasoning": "Simple network diagnostic"
                },
                "categories": {
                    "categories": ["network"],
                    "reasoning": "Network connectivity check"
                },
                "dependencies": {
                    "is_multi_step": false,
                    "dependencies": [],
                    "workflow_steps": ["ping"],
                    "reasoning": "Single network operation"
                },
                "extracted_entities": {
                    "entities": {"target": "google.com"},
                    "typed_entities": {"hostname": ["google.com"]},
                    "extraction_confidence": 0.9,
                    "extraction_reasoning": "Extracted target hostname"
                }
            }"#
            .to_string(),
        }
    }
}

#[async_trait]
impl QueryAnalysisLLM for MockLLM {
    async fn generate_analysis(&self, _prompt: &str) -> Result<SimplifiedLLMResponse> {
        Ok(SimplifiedLLMResponse::content_only(self.response.clone()))
    }
}

/// Mock tool discovery
struct MockToolDiscovery;

#[async_trait]
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
        vec!["network".to_string(), "cloud".to_string()]
    }
}

async fn create_test_analyzer(mock_llm: MockLLM) -> Arc<UnifiedQueryAnalyzer> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(manifest_dir);
    let data_dir = repo_root.join("data/magician_v2/prompts");
    let config = JsonStorageConfig {
        storage_dir: data_dir,
        enable_cache: false,
        max_cache_entries: 1,
    };

    let storage = JsonPromptStorage::new(config).unwrap();
    let prompt_manager = Arc::new(PromptManager::new(Arc::new(storage)));
    let tool_discovery = Arc::new(MockToolDiscovery) as Arc<dyn ToolDiscovery>;

    // Wrap ToolDiscovery into ToolCatalog using adapter
    let tool_adapter = Arc::new(magician::magician_v2::tooling::ToolDiscoveryAdapter::new(
        tool_discovery,
    ));
    let tool_catalog: Arc<dyn runtime_core::ToolCatalog> = tool_adapter.clone();

    Arc::new(UnifiedQueryAnalyzer::new(
        Arc::new(mock_llm),
        tool_catalog,
        prompt_manager,
    ))
}

#[tokio::test]
async fn test_consolidated_analysis_with_intent_detection() {
    // Create analyzer with mock that returns intent detection
    let mock_llm = MockLLM::new_with_intent_detection();
    let analyzer = create_test_analyzer(mock_llm).await;

    // Create conversation context simulating an execution waiting for region input
    let conversation_context = ConversationContext {
        execution_status: WaitingState::WaitingUser,
        pending_slots: vec![V2Slot {
            id: "slot_region".to_string(),
            execution_id: "exec_123".to_string(),
            name: "region".to_string(),
            required: true,
            status: V2SlotStatus::Pending,
            asked_turn_id: None,
            answer: None,
            schema_json: serde_json::json!({
                "type": "string",
                "description": "AWS region"
            }),
            created_at: 0,
            updated_at: 0,
        }],
        recent_turns: vec![V2Turn {
            id: "turn_1".to_string(),
            execution_id: "exec_123".to_string(),
            direction: TurnDirection::Outbound,
            text: "Which AWS region would you like to deploy to?".to_string(),
            in_reply_to_slot_id: Some("slot_region".to_string()),
            created_at: 0,
            query_analysis: None,
            analysis_metadata: None,
            strategy_attempts: vec![],
            processing_metadata: None,
            recommended_questions: None,
            enriched_query: None,
        }],
    };

    let exec_context = ExecutionContext::default();

    // Perform consolidated analysis
    let result = analyzer
        .analyze_query_with_context(
            "us-west-2",
            &exec_context,
            None,
            conversation_context,
            None,
            None,
        )
        .await;

    let analysis = result.expect("Analysis should succeed (intent detection)");

    // Verify intent was detected
    assert_eq!(
        analysis.intent,
        QueryIntent::AnswerElicitation,
        "Should detect AnswerElicitation intent"
    );

    // Verify slot match was performed
    assert!(analysis.slot_match.is_some(), "Should have slot match");
    let slot_match = analysis.slot_match.unwrap();
    assert_eq!(slot_match.slot_id, "slot_region");
    assert_eq!(slot_match.extracted_value, serde_json::json!("us-west-2"));
    assert!(
        slot_match.match_confidence > 0.9,
        "Should have high confidence"
    );

    // Verify query analysis was also performed
    assert!(analysis.complexity.score >= 0.0 && analysis.complexity.score <= 1.0);
    assert!(!analysis.categories.categories.is_empty());

    println!("✅ Consolidated analysis with intent detection successful!");
    println!("   Intent: {:?}", analysis.intent);
    println!("   Slot matched: {}", slot_match.slot_id);
    println!("   Confidence: {:.2}", slot_match.match_confidence);
}

#[tokio::test]
async fn test_consolidated_analysis_new_task() {
    // Create analyzer with mock that returns NewTask intent
    let mock_llm = MockLLM::new_with_new_task();
    let analyzer = create_test_analyzer(mock_llm).await;

    // Create conversation context simulating an active execution
    let conversation_context = ConversationContext {
        execution_status: WaitingState::Planning,
        pending_slots: vec![],
        recent_turns: vec![],
    };

    let exec_context = ExecutionContext::default();

    // Perform consolidated analysis
    let result = analyzer
        .analyze_query_with_context(
            "ping google.com",
            &exec_context,
            None,
            conversation_context,
            None,
            None,
        )
        .await;

    let analysis = result.expect("Analysis should succeed (new task)");

    // Verify intent was detected as NewTask
    assert_eq!(
        analysis.intent,
        QueryIntent::NewTask,
        "Should detect NewTask intent"
    );

    // Verify no slot match for new task
    assert!(
        analysis.slot_match.is_none(),
        "Should not have slot match for new task"
    );

    // Verify query analysis
    assert_eq!(analysis.categories.categories, vec!["network"]);
    assert!(!analysis.extracted_entities.entities.is_empty());
    assert_eq!(
        analysis.extracted_entities.entities.get("target"),
        Some(&"google.com".to_string())
    );

    println!("✅ Consolidated analysis for new task successful!");
    println!("   Intent: {:?}", analysis.intent);
    println!("   Categories: {:?}", analysis.categories.categories);
    println!("   Entities: {:?}", analysis.extracted_entities.entities);
}

#[tokio::test]
async fn test_context_free_analysis_still_works() {
    // Verify backward compatibility with context-free analysis
    let mock_llm = MockLLM::new_with_new_task();
    let analyzer = create_test_analyzer(mock_llm).await;

    let exec_context = ExecutionContext::default();

    // Perform context-free analysis (original method)
    let result = analyzer
        .analyze_query("ping google.com", &exec_context, None, None, None)
        .await;

    let analysis = result.expect("Context-free analysis should still work");

    // Verify it defaults to NewTask when no context
    assert_eq!(
        analysis.intent,
        QueryIntent::NewTask,
        "Should default to NewTask without context"
    );

    // Verify no slot match without context
    assert!(
        analysis.slot_match.is_none(),
        "Should not have slot match without context"
    );

    // Verify query analysis still works
    assert!(!analysis.categories.categories.is_empty());

    println!("✅ Backward compatibility verified - context-free analysis works!");
}
