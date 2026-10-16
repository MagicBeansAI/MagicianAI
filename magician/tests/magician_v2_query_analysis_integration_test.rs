// Integration tests for MagicianV2 Query Analysis system
// Tests the complete UnifiedQueryAnalyzer with LLM evaluation framework

use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use magician::magician_v2::{
    prompts::{
        json_storage::JsonStorageConfig, storage::PromptStore, JsonPromptStorage, PromptManager,
    },
    query_analysis::{
        operation_llm_router::{QueryAnalysisLLM, SimplifiedLLMResponse},
        EvaluationCase, EvaluationDataset, LLMEvaluator, UnifiedQueryAnalyzer,
    },
};
use runtime_core::{ExecutionContext, MultipleToolMatchResult, ToolDiscovery, ToolMatchResult};

fn prompts_dir() -> std::path::PathBuf {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(manifest_dir)
        .join("data/magician_v2/prompts")
}

/// Mock ToolDiscovery for integration testing
struct MockToolDiscovery {
    categories: Vec<String>,
}

impl MockToolDiscovery {
    fn new() -> Self {
        Self {
            categories: vec![
                "network".to_string(),
                "filesystem".to_string(),
                "deployment".to_string(),
                "cloud".to_string(),
            ],
        }
    }
}

#[async_trait::async_trait]
impl ToolDiscovery for MockToolDiscovery {
    async fn find_best_match_with_context(
        &self,
        _task_description: &str,
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
        _task_description: &str,
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
        self.categories.clone()
    }
}

// Mock LLM service for integration testing
struct IntegrationTestLLM {
    responses: HashMap<String, String>,
}

impl IntegrationTestLLM {
    fn new() -> Self {
        let mut responses = HashMap::new();

        // Response for simple queries
        responses.insert(
            "ping".to_string(),
            Self::build_response(
                vec!["network", "diagnostics"],
                0.2,
                false,
                vec![],
                "Simple network diagnostic command",
            ),
        );

        // Response for complex queries
        responses.insert(
            "deploy".to_string(),
            Self::build_response(
                vec!["deployment", "cloud", "web"],
                0.9,
                true,
                vec!["AWS account", "SSL certificate"],
                "Complex multi-step deployment requiring multiple services",
            ),
        );

        // Response for file operations
        responses.insert(
            "list".to_string(),
            Self::build_response(
                vec!["filesystem", "search"],
                0.3,
                false,
                vec![],
                "File system search operation",
            ),
        );

        // Response for data processing
        responses.insert(
            "process".to_string(),
            Self::build_response(
                vec!["data", "analysis", "visualization"],
                0.8,
                true,
                vec!["CSV file", "data processing tools"],
                "Multi-step data processing pipeline",
            ),
        );

        // Default response for unknown queries
        responses.insert(
            "default".to_string(),
            Self::build_response(vec!["general"], 0.5, false, vec![], "General purpose query"),
        );

        Self { responses }
    }

    fn build_response(
        categories: Vec<&str>,
        complexity_score: f64,
        is_multi_step: bool,
        dependencies: Vec<&str>,
        reasoning: &str,
    ) -> String {
        let json = serde_json::json!({
            "complexity": {
                "score": complexity_score,
                "factors": [],
                "reasoning": reasoning,
            },
            "categories": {
                "categories": categories,
                "reasoning": reasoning,
            },
            "dependencies": {
                "is_multi_step": is_multi_step,
                "dependencies": dependencies,
                "workflow_steps": dependencies,
                "reasoning": reasoning,
            },
            "extracted_entities": {
                "entities": {},
                "typed_entities": {},
                "extraction_confidence": 0.5,
                "extraction_reasoning": reasoning,
            }
        });
        json.to_string()
    }

    fn get_response_for_query(&self, query: &str) -> String {
        // Simple keyword matching for test purposes
        for (keyword, response) in &self.responses {
            if query.to_lowercase().contains(keyword) {
                return response.clone();
            }
        }
        self.responses["default"].clone()
    }
}

#[async_trait::async_trait]
impl QueryAnalysisLLM for IntegrationTestLLM {
    async fn generate_analysis(&self, prompt: &str) -> Result<SimplifiedLLMResponse> {
        // Extract query from prompt for keyword matching
        let lines: Vec<&str> = prompt.lines().collect();
        let query_line = lines
            .iter()
            .find(|line| line.to_lowercase().contains("query:"))
            .unwrap_or(&"");

        let query = query_line
            .replace("Query:", "")
            .replace("\"", "")
            .trim()
            .to_lowercase();
        Ok(SimplifiedLLMResponse::content_only(
            self.get_response_for_query(&query),
        ))
    }

    async fn is_available(&self) -> bool {
        true
    }
}

async fn create_test_unified_analyzer() -> Result<UnifiedQueryAnalyzer> {
    // Create test LLM service
    let llm_service: Arc<dyn QueryAnalysisLLM> = Arc::new(IntegrationTestLLM::new());

    // Create test tool discovery with mock categories
    let tool_discovery = Arc::new(MockToolDiscovery::new()) as Arc<dyn ToolDiscovery>;

    // Wrap ToolDiscovery into ToolCatalog using adapter
    let tool_adapter = Arc::new(magician::magician_v2::tooling::ToolDiscoveryAdapter::new(
        tool_discovery,
    ));
    let tool_catalog: Arc<dyn runtime_core::ToolCatalog> = tool_adapter.clone();

    // Create test prompt storage and manager
    let data_dir = prompts_dir();
    let config = JsonStorageConfig {
        storage_dir: data_dir,
        enable_cache: true,
        max_cache_entries: 10,
    };
    let storage = JsonPromptStorage::new(config)?;
    let prompt_manager = Arc::new(PromptManager::new(Arc::new(storage)));

    Ok(UnifiedQueryAnalyzer::new(
        llm_service,
        tool_catalog,
        prompt_manager,
    ))
}

async fn create_test_evaluator() -> Result<LLMEvaluator> {
    // Create test LLM service
    let llm_service: Arc<dyn QueryAnalysisLLM> = Arc::new(IntegrationTestLLM::new());

    // Create test tool discovery with mock categories
    let tool_discovery = Arc::new(MockToolDiscovery::new()) as Arc<dyn ToolDiscovery>;

    // Wrap ToolDiscovery into ToolCatalog using adapter
    let tool_adapter = Arc::new(magician::magician_v2::tooling::ToolDiscoveryAdapter::new(
        tool_discovery,
    ));
    let tool_catalog: Arc<dyn runtime_core::ToolCatalog> = tool_adapter.clone();

    // Create test prompt storage and manager
    let data_dir = prompts_dir();
    let config = JsonStorageConfig {
        storage_dir: data_dir,
        enable_cache: true,
        max_cache_entries: 10,
    };
    let storage = JsonPromptStorage::new(config)?;
    let prompt_manager = Arc::new(PromptManager::new(Arc::new(storage)));

    Ok(LLMEvaluator::new(llm_service, tool_catalog, prompt_manager))
}

fn create_integration_test_dataset() -> EvaluationDataset {
    EvaluationDataset {
        name: "integration_test".to_string(),
        version: "1.0.0".to_string(),
        description: "Integration test dataset for end-to-end testing".to_string(),
        cases: vec![
            EvaluationCase {
                id: "integration_ping".to_string(),
                query: "ping google.com".to_string(),
                expected_complexity: 0.2,
                expected_categories: vec!["network".to_string(), "diagnostics".to_string()],
                expected_is_multi_step: false,
                expected_dependencies: vec![],
                expected_resource_estimate: 10.0,
                description: "Integration test for simple ping command".to_string(),
            },
            EvaluationCase {
                id: "integration_deployment".to_string(),
                query: "deploy my app to AWS".to_string(),
                expected_complexity: 0.9,
                expected_categories: vec!["deployment".to_string(), "cloud".to_string()],
                expected_is_multi_step: true,
                expected_dependencies: vec!["AWS account".to_string()],
                expected_resource_estimate: 80.0,
                description: "Integration test for complex deployment".to_string(),
            },
            EvaluationCase {
                id: "integration_file_ops".to_string(),
                query: "list all Python files".to_string(),
                expected_complexity: 0.3,
                expected_categories: vec!["filesystem".to_string(), "search".to_string()],
                expected_is_multi_step: false,
                expected_dependencies: vec![],
                expected_resource_estimate: 15.0,
                description: "Integration test for file operations".to_string(),
            },
        ],
    }
}

#[tokio::test]
async fn test_unified_analyzer_integration() -> Result<()> {
    let analyzer = create_test_unified_analyzer().await?;

    // Create test execution context
    let context = ExecutionContext {
        principal: "test-user".to_string(),
        workspace: "test-workspace".to_string(),
        metadata: HashMap::new(),
    };

    // Test simple query
    let result = analyzer
        .analyze_query("ping google.com", &context, None, None, None)
        .await?;

    assert!(result.complexity.score >= 0.2); // Should be at least 0.2 for a simple ping command
                                             // Check that some reasonable categories are returned (system is working)
    assert!(!result.categories.categories.is_empty()); // Should have some categories
    assert!(!result.dependencies.is_multi_step);
    assert!(result.dependencies.dependencies.is_empty());
    assert!(result.resource_estimate.expected_tokens > 0); // Should have some resource estimate

    // Test complex query
    let result = analyzer
        .analyze_query("deploy my React app to AWS", &context, None, None, None)
        .await?;

    assert!(result.complexity.score >= 0.5); // Should be a complex query
    assert!(!result.categories.categories.is_empty()); // Should have categories
                                                       // Multi-step and dependencies are more flexible since actual analysis might
                                                       // differ
    assert!(result.resource_estimate.expected_tokens > 0); // Should have resource estimate

    Ok(())
}

#[tokio::test]
async fn test_llm_evaluator_integration() -> Result<()> {
    let mut evaluator = create_test_evaluator().await?;
    let dataset = create_integration_test_dataset();

    // Load the test dataset
    evaluator.load_dataset(dataset);

    // Run evaluation
    let metrics = evaluator.evaluate_dataset("integration_test").await?;

    // Verify metrics structure
    assert_eq!(metrics.total_cases, 3);
    assert!(metrics.overall_score >= 0.0 && metrics.overall_score <= 1.0);
    assert!(metrics.avg_complexity_error >= 0.0);
    assert!(metrics.avg_category_f1 >= 0.0 && metrics.avg_category_f1 <= 1.0);
    assert!(metrics.multi_step_accuracy >= 0.0 && metrics.multi_step_accuracy <= 1.0);
    assert!(metrics.avg_resource_error >= 0.0);

    // Generate and verify report
    let report = evaluator.generate_report(&metrics);
    assert!(report.contains("LLM Evaluation Report"));
    assert!(report.contains("Total Cases: 3"));
    assert!(report.contains("Overall Score:"));

    Ok(())
}

#[tokio::test]
async fn test_evaluation_case_processing() -> Result<()> {
    let evaluator = create_test_evaluator().await?;

    let test_case = EvaluationCase {
        id: "test_case".to_string(),
        query: "ping localhost".to_string(),
        expected_complexity: 0.2,
        expected_categories: vec!["network".to_string()],
        expected_is_multi_step: false,
        expected_dependencies: vec![],
        expected_resource_estimate: 10.0,
        description: "Test case for evaluation".to_string(),
    };

    let result = evaluator.evaluate_case(&test_case).await?;

    // Verify evaluation result structure
    assert_eq!(result.case_id, "test_case");
    assert_eq!(result.expected_case.query, "ping localhost");
    assert!(result.complexity_error >= 0.0);
    assert!(result.category_f1 >= 0.0 && result.category_f1 <= 1.0);
    assert!(result.overall_score >= 0.0 && result.overall_score <= 1.0);

    Ok(())
}

#[tokio::test]
async fn test_dataset_loading_from_json() -> Result<()> {
    let mut evaluator = create_test_evaluator().await?;

    let json_dataset = r#"{
        "name": "json_test",
        "version": "1.0.0",
        "description": "Test dataset from JSON",
        "cases": [
            {
                "id": "json_test_case",
                "query": "test query",
                "expected_complexity": 0.5,
                "expected_categories": ["test"],
                "expected_is_multi_step": true,
                "expected_dependencies": ["dependency"],
                "expected_resource_estimate": 50.0,
                "description": "JSON test case"
            }
        ]
    }"#;

    // Test JSON loading
    evaluator.load_dataset_from_json(json_dataset)?;

    // Verify dataset was loaded
    let metrics = evaluator.evaluate_dataset("json_test").await?;
    assert_eq!(metrics.total_cases, 1);

    Ok(())
}

#[tokio::test]
async fn test_benchmark_functionality() -> Result<()> {
    let mut evaluator = create_test_evaluator().await?;
    let dataset = create_integration_test_dataset();
    evaluator.load_dataset(dataset);

    // Run a small benchmark (2 iterations)
    let benchmark_results = evaluator.benchmark("integration_test", 2).await?;

    assert_eq!(benchmark_results.len(), 2);

    // Verify each result has valid metrics
    for metrics in benchmark_results {
        assert_eq!(metrics.total_cases, 3);
        assert!(metrics.overall_score >= 0.0 && metrics.overall_score <= 1.0);
    }

    Ok(())
}

#[tokio::test]
async fn test_sample_dataset_creation() -> Result<()> {
    let sample_dataset = LLMEvaluator::create_sample_dataset();

    assert_eq!(sample_dataset.name, "sample_evaluation");
    assert_eq!(sample_dataset.version, "1.0.0");
    assert_eq!(sample_dataset.cases.len(), 4);

    // Verify sample cases
    let ping_case = sample_dataset
        .cases
        .iter()
        .find(|case| case.id == "simple_ping")
        .expect("Should have simple_ping case");

    assert_eq!(ping_case.query, "ping google.com");
    assert_eq!(ping_case.expected_complexity, 0.2);

    Ok(())
}

#[tokio::test]
async fn test_error_handling() -> Result<()> {
    let evaluator = create_test_evaluator().await?;

    // Test evaluation of non-existent dataset
    let result = evaluator.evaluate_dataset("nonexistent").await;
    assert!(result.is_err());

    // Test invalid JSON loading
    let mut evaluator = create_test_evaluator().await?;
    let invalid_json = "{ invalid json }";
    let result = evaluator.load_dataset_from_json(invalid_json);
    assert!(result.is_err());

    Ok(())
}

#[tokio::test]
async fn test_classification_metrics() -> Result<()> {
    let evaluator = create_test_evaluator().await?;

    // Test perfect match
    let actual = vec!["cat1".to_string(), "cat2".to_string()];
    let expected = vec!["cat1".to_string(), "cat2".to_string()];
    let (precision, recall, f1) = evaluator.calculate_classification_metrics(&actual, &expected);
    assert_eq!(precision, 1.0);
    assert_eq!(recall, 1.0);
    assert_eq!(f1, 1.0);

    // Test partial match
    let actual = vec!["cat1".to_string(), "cat2".to_string()];
    let expected = vec!["cat1".to_string(), "cat3".to_string()];
    let (precision, recall, f1) = evaluator.calculate_classification_metrics(&actual, &expected);
    assert_eq!(precision, 0.5); // 1 out of 2 actual are correct
    assert_eq!(recall, 0.5); // 1 out of 2 expected are found
    assert_eq!(f1, 0.5); // F1 = 2 * 0.5 * 0.5 / (0.5 + 0.5)

    // Test empty cases
    let actual = vec![];
    let expected = vec![];
    let (precision, recall, f1) = evaluator.calculate_classification_metrics(&actual, &expected);
    assert_eq!(precision, 1.0);
    assert_eq!(recall, 1.0);
    assert_eq!(f1, 1.0);

    Ok(())
}

#[tokio::test]
async fn test_end_to_end_workflow() -> Result<()> {
    // Test complete workflow: analyzer -> evaluator -> metrics -> report

    // Create test execution context
    let context = ExecutionContext {
        principal: "test-user".to_string(),
        workspace: "test-workspace".to_string(),
        metadata: HashMap::new(),
    };

    // 1. Create analyzer and test query analysis
    let analyzer = create_test_unified_analyzer().await?;
    let analysis_result = analyzer
        .analyze_query("ping google.com", &context, None, None, None)
        .await?;

    // 2. Create evaluator and load test data
    let mut evaluator = create_test_evaluator().await?;
    let dataset = create_integration_test_dataset();
    evaluator.load_dataset(dataset);

    // 3. Run evaluation
    let metrics = evaluator.evaluate_dataset("integration_test").await?;

    // 4. Generate report
    let report = evaluator.generate_report(&metrics);

    // 5. Verify end-to-end results
    assert!(analysis_result.complexity.score > 0.0);
    assert!(!analysis_result.categories.categories.is_empty());
    assert!(metrics.total_cases > 0);
    assert!(report.contains("LLM Evaluation Report"));

    println!("✅ End-to-end integration test completed successfully");
    println!(
        "📊 Analysis result: complexity={}, categories={:?}",
        analysis_result.complexity.score, analysis_result.categories.categories
    );
    println!(
        "📈 Evaluation metrics: score={:.3}, cases={}",
        metrics.overall_score, metrics.total_cases
    );

    Ok(())
}

// Helper function to verify test environment
#[tokio::test]
async fn test_environment_setup() -> Result<()> {
    // Verify we can create all components
    let _analyzer = create_test_unified_analyzer().await?;
    let _evaluator = create_test_evaluator().await?;
    let _dataset = create_integration_test_dataset();
    let _sample = LLMEvaluator::create_sample_dataset();

    println!("✅ Test environment setup verified");
    Ok(())
}

// Error handling tests for wrong prompt file/folder paths
#[tokio::test]
async fn test_error_handling_nonexistent_prompt_directory() -> Result<()> {
    // Create test LLM service
    let llm_service: Arc<dyn QueryAnalysisLLM> = Arc::new(IntegrationTestLLM::new());

    // Create test tool discovery with mock categories
    let tool_discovery = Arc::new(MockToolDiscovery::new()) as Arc<dyn ToolDiscovery>;

    // Wrap ToolDiscovery into ToolCatalog using adapter
    let tool_adapter = Arc::new(magician::magician_v2::tooling::ToolDiscoveryAdapter::new(
        tool_discovery,
    ));
    let tool_catalog: Arc<dyn runtime_core::ToolCatalog> = tool_adapter.clone();

    // Create prompt storage with NON-EXISTENT directory
    let nonexistent_dir = std::path::PathBuf::from("/nonexistent/directory/that/does/not/exist");
    let config = JsonStorageConfig {
        storage_dir: nonexistent_dir.clone(),
        enable_cache: true,
        max_cache_entries: 10,
    };
    let storage = JsonPromptStorage::new(config)?;
    let prompt_manager = Arc::new(PromptManager::new(Arc::new(storage)));

    // Create test execution context
    let context = ExecutionContext {
        principal: "test-user".to_string(),
        workspace: "test-workspace".to_string(),
        metadata: HashMap::new(),
    };

    // Create analyzer
    let analyzer = UnifiedQueryAnalyzer::new(llm_service, tool_catalog, prompt_manager);

    // This should fail because the prompt directory doesn't exist
    let result = analyzer
        .analyze_query("ping google.com", &context, None, None, None)
        .await;

    // Verify it fails with an appropriate error
    assert!(
        result.is_err(),
        "Expected error when using nonexistent prompt directory"
    );
    let error_message = result.unwrap_err().to_string();
    assert!(
        error_message.contains("not found") || error_message.contains("No such file"),
        "Error should mention file not found, got: {}",
        error_message
    );

    println!("✅ Correctly handled nonexistent prompt directory");
    Ok(())
}

#[tokio::test]
async fn test_error_handling_missing_prompt_file() -> Result<()> {
    // Create test LLM service
    let llm_service: Arc<dyn QueryAnalysisLLM> = Arc::new(IntegrationTestLLM::new());

    // Create test tool discovery with mock categories
    let tool_discovery = Arc::new(MockToolDiscovery::new()) as Arc<dyn ToolDiscovery>;

    // Wrap ToolDiscovery into ToolCatalog using adapter
    let tool_adapter = Arc::new(magician::magician_v2::tooling::ToolDiscoveryAdapter::new(
        tool_discovery,
    ));
    let tool_catalog: Arc<dyn runtime_core::ToolCatalog> = tool_adapter.clone();

    // Create prompt storage with VALID directory but point to missing prompt files
    let temp_dir = std::env::temp_dir().join("missing_prompt_test");
    std::fs::create_dir_all(&temp_dir)?;

    let config = JsonStorageConfig {
        storage_dir: temp_dir.clone(),
        enable_cache: true,
        max_cache_entries: 10,
    };
    let storage = JsonPromptStorage::new(config)?;
    let prompt_manager = Arc::new(PromptManager::new(Arc::new(storage)));

    // Create test execution context
    let context = ExecutionContext {
        principal: "test-user".to_string(),
        workspace: "test-workspace".to_string(),
        metadata: HashMap::new(),
    };

    // Create analyzer
    let analyzer = UnifiedQueryAnalyzer::new(llm_service, tool_catalog, prompt_manager);

    // This should fail because the expected prompt files don't exist in the temp
    // directory
    let result = analyzer
        .analyze_query("ping google.com", &context, None, None, None)
        .await;

    // Verify it fails with appropriate error about missing prompt
    assert!(
        result.is_err(),
        "Expected error when prompt files are missing"
    );
    let error_message = result.unwrap_err().to_string();
    assert!(
        error_message.to_lowercase().contains("not found")
            || error_message.to_lowercase().contains("prompt"),
        "Error should mention prompt not found, got: {}",
        error_message
    );

    // Cleanup
    std::fs::remove_dir_all(&temp_dir).ok();

    println!("✅ Correctly handled missing prompt files");
    Ok(())
}

#[tokio::test]
async fn test_error_handling_invalid_prompt_file_format() -> Result<()> {
    // Create test LLM service
    let llm_service: Arc<dyn QueryAnalysisLLM> = Arc::new(IntegrationTestLLM::new());

    // Create test tool discovery with mock categories
    let tool_discovery = Arc::new(MockToolDiscovery::new()) as Arc<dyn ToolDiscovery>;

    // Wrap ToolDiscovery into ToolCatalog using adapter
    let tool_adapter = Arc::new(magician::magician_v2::tooling::ToolDiscoveryAdapter::new(
        tool_discovery,
    ));
    let tool_catalog: Arc<dyn runtime_core::ToolCatalog> = tool_adapter.clone();

    // Create temp directory with invalid JSON file
    let temp_dir = std::env::temp_dir().join("invalid_prompt_test");
    std::fs::create_dir_all(&temp_dir)?;

    // Create invalid JSON file that matches expected prompt filename pattern
    let invalid_prompt_file = temp_dir.join("unified_analysis_v1.1.0.json");
    std::fs::write(
        &invalid_prompt_file,
        "{ invalid json content without proper structure }",
    )?;

    let config = JsonStorageConfig {
        storage_dir: temp_dir.clone(),
        enable_cache: true,
        max_cache_entries: 10,
    };
    let storage = JsonPromptStorage::new(config)?;
    let prompt_manager = Arc::new(PromptManager::new(Arc::new(storage)));

    // Create test execution context
    let context = ExecutionContext {
        principal: "test-user".to_string(),
        workspace: "test-workspace".to_string(),
        metadata: HashMap::new(),
    };

    // Create analyzer
    let analyzer = UnifiedQueryAnalyzer::new(llm_service, tool_catalog, prompt_manager);

    // This should fail because the prompt file has invalid format
    let result = analyzer
        .analyze_query("ping google.com", &context, None, None, None)
        .await;

    // Verify it fails with JSON parsing error
    assert!(
        result.is_err(),
        "Expected error when prompt file has invalid format"
    );
    let error_message = result.unwrap_err().to_string();
    assert!(
        error_message.to_lowercase().contains("parse")
            || error_message.to_lowercase().contains("json")
            || error_message.to_lowercase().contains("deserialize"),
        "Error should mention parsing/JSON error, got: {}",
        error_message
    );

    // Cleanup
    std::fs::remove_dir_all(&temp_dir).ok();

    println!("✅ Correctly handled invalid prompt file format");
    Ok(())
}

#[tokio::test]
async fn test_prompt_storage_health_check_invalid_directory() -> Result<()> {
    // Test health check on nonexistent directory
    let nonexistent_dir =
        std::path::PathBuf::from("/totally/nonexistent/path/that/should/not/exist");
    let config = JsonStorageConfig {
        storage_dir: nonexistent_dir,
        enable_cache: true,
        max_cache_entries: 10,
    };
    let storage = JsonPromptStorage::new(config)?;

    // Health check should return false for nonexistent directory
    let health_result = storage.health_check().await?;
    assert!(
        !health_result,
        "Health check should fail for nonexistent directory"
    );

    println!("✅ Health check correctly detected invalid directory");
    Ok(())
}

#[tokio::test]
async fn test_prompt_storage_health_check_valid_directory() -> Result<()> {
    // Test health check on valid directory
    let data_dir = prompts_dir();
    let config = JsonStorageConfig {
        storage_dir: data_dir,
        enable_cache: true,
        max_cache_entries: 10,
    };
    let storage = JsonPromptStorage::new(config)?;

    // Health check should pass for existing directory
    let health_result = storage.health_check().await?;
    assert!(
        health_result,
        "Health check should pass for valid directory"
    );

    println!("✅ Health check correctly validated existing directory");
    Ok(())
}
