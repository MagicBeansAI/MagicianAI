// LLM Evaluation Framework for UnifiedQueryAnalyzer
// Provides comprehensive testing and evaluation capabilities

use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::unified_analyzer::{UnifiedQueryAnalysis, UnifiedQueryAnalyzer};
use crate::magician_v2::{
    prompts::PromptManager, query_analysis::operation_llm_router::QueryAnalysisLLM,
};
use runtime_core::{ExecutionContext, ToolCatalog};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationCase {
    pub id: String,
    pub query: String,
    pub expected_complexity: f64,
    pub expected_categories: Vec<String>,
    pub expected_is_multi_step: bool,
    pub expected_dependencies: Vec<String>,
    pub expected_resource_estimate: f64,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationDataset {
    pub name: String,
    pub version: String,
    pub description: String,
    pub cases: Vec<EvaluationCase>,
}

#[derive(Debug, Clone)]
pub struct EvaluationResult {
    pub case_id: String,
    pub actual_result: UnifiedQueryAnalysis,
    pub expected_case: EvaluationCase,
    pub complexity_error: f64,
    pub category_precision: f64,
    pub category_recall: f64,
    pub category_f1: f64,
    pub multi_step_correct: bool,
    pub dependency_precision: f64,
    pub dependency_recall: f64,
    pub dependency_f1: f64,
    pub resource_error: f64,
    pub overall_score: f64,
}

#[derive(Debug, Clone)]
pub struct EvaluationMetrics {
    pub total_cases: usize,
    pub passed_cases: usize,
    pub avg_complexity_error: f64,
    pub avg_category_precision: f64,
    pub avg_category_recall: f64,
    pub avg_category_f1: f64,
    pub multi_step_accuracy: f64,
    pub avg_dependency_precision: f64,
    pub avg_dependency_recall: f64,
    pub avg_dependency_f1: f64,
    pub avg_resource_error: f64,
    pub overall_score: f64,
}

pub struct LLMEvaluator {
    analyzer: UnifiedQueryAnalyzer,
    datasets: HashMap<String, EvaluationDataset>,
}

impl LLMEvaluator {
    pub fn new(
        llm_service: Arc<dyn QueryAnalysisLLM>,
        tool_catalog: Arc<dyn ToolCatalog>,
        prompt_manager: Arc<PromptManager>,
    ) -> Self {
        let analyzer = UnifiedQueryAnalyzer::new(llm_service, tool_catalog, prompt_manager);

        Self {
            analyzer,
            datasets: HashMap::new(),
        }
    }

    pub fn load_dataset(&mut self, dataset: EvaluationDataset) {
        self.datasets.insert(dataset.name.clone(), dataset);
    }

    pub fn load_dataset_from_json(&mut self, json_content: &str) -> Result<()> {
        let dataset: EvaluationDataset = serde_json::from_str(json_content)?;
        self.load_dataset(dataset);
        Ok(())
    }

    pub async fn evaluate_dataset(&self, dataset_name: &str) -> Result<EvaluationMetrics> {
        let dataset = self
            .datasets
            .get(dataset_name)
            .ok_or_else(|| anyhow::anyhow!("Dataset '{}' not found", dataset_name))?;

        let mut results = Vec::new();

        for case in &dataset.cases {
            let result = self.evaluate_case(case).await?;
            results.push(result);
        }

        Ok(self.calculate_metrics(results))
    }

    pub async fn evaluate_case(&self, case: &EvaluationCase) -> Result<EvaluationResult> {
        // Create default execution context for evaluation
        let context = ExecutionContext::default();
        let actual_result = self
            .analyzer
            .analyze_query(&case.query, &context, None, None, None)
            .await?;

        // Calculate complexity error (absolute difference)
        let complexity_error =
            (actual_result.complexity.score as f64 - case.expected_complexity).abs();

        // Calculate category metrics
        let (category_precision, category_recall, category_f1) = self
            .calculate_classification_metrics(
                &actual_result.categories.categories,
                &case.expected_categories,
            );

        // Check multi-step prediction
        let multi_step_correct =
            actual_result.dependencies.is_multi_step == case.expected_is_multi_step;

        // Calculate dependency metrics
        let (dependency_precision, dependency_recall, dependency_f1) = self
            .calculate_classification_metrics(
                &actual_result.dependencies.dependencies,
                &case.expected_dependencies,
            );

        // Calculate resource estimation error
        let resource_error = (actual_result.resource_estimate.expected_tokens as f64
            - case.expected_resource_estimate)
            .abs();

        // Calculate overall score (weighted combination)
        let overall_score = self.calculate_overall_score(
            complexity_error,
            category_f1,
            multi_step_correct,
            dependency_f1,
            resource_error,
        );

        Ok(EvaluationResult {
            case_id: case.id.clone(),
            actual_result,
            expected_case: case.clone(),
            complexity_error,
            category_precision,
            category_recall,
            category_f1,
            multi_step_correct,
            dependency_precision,
            dependency_recall,
            dependency_f1,
            resource_error,
            overall_score,
        })
    }

    pub fn calculate_classification_metrics(
        &self,
        actual: &[String],
        expected: &[String],
    ) -> (f64, f64, f64) {
        if expected.is_empty() && actual.is_empty() {
            return (1.0, 1.0, 1.0);
        }

        if expected.is_empty() {
            return (0.0, 1.0, 0.0);
        }

        if actual.is_empty() {
            return (1.0, 0.0, 0.0);
        }

        let actual_set: std::collections::HashSet<_> = actual.iter().collect();
        let expected_set: std::collections::HashSet<_> = expected.iter().collect();

        let intersection = actual_set.intersection(&expected_set).count() as f64;
        let precision = intersection / actual.len() as f64;
        let recall = intersection / expected.len() as f64;

        let f1 = if precision + recall > 0.0 {
            2.0 * precision * recall / (precision + recall)
        } else {
            0.0
        };

        (precision, recall, f1)
    }

    fn calculate_overall_score(
        &self,
        complexity_error: f64,
        category_f1: f64,
        multi_step_correct: bool,
        dependency_f1: f64,
        resource_error: f64,
    ) -> f64 {
        // Normalize complexity error (assume max error of 1.0)
        let complexity_score = (1.0 - complexity_error.min(1.0)).max(0.0);

        // Multi-step score
        let multi_step_score = if multi_step_correct { 1.0 } else { 0.0 };

        // Normalize resource error (assume max error of 100.0)
        let resource_score = (1.0 - (resource_error / 100.0).min(1.0)).max(0.0);

        // Weighted combination
        let weights = [0.2, 0.3, 0.2, 0.2, 0.1]; // complexity, category, multi_step, dependency, resource
        let scores = [
            complexity_score,
            category_f1,
            multi_step_score,
            dependency_f1,
            resource_score,
        ];

        weights.iter().zip(scores.iter()).map(|(w, s)| w * s).sum()
    }

    fn calculate_metrics(&self, results: Vec<EvaluationResult>) -> EvaluationMetrics {
        let total_cases = results.len();
        let passed_cases = results
            .iter()
            .filter(|r| r.overall_score >= 0.8) // 80% threshold for "passed"
            .count();

        let avg_complexity_error =
            results.iter().map(|r| r.complexity_error).sum::<f64>() / total_cases as f64;

        let avg_category_precision =
            results.iter().map(|r| r.category_precision).sum::<f64>() / total_cases as f64;

        let avg_category_recall =
            results.iter().map(|r| r.category_recall).sum::<f64>() / total_cases as f64;

        let avg_category_f1 =
            results.iter().map(|r| r.category_f1).sum::<f64>() / total_cases as f64;

        let multi_step_accuracy =
            results.iter().filter(|r| r.multi_step_correct).count() as f64 / total_cases as f64;

        let avg_dependency_precision =
            results.iter().map(|r| r.dependency_precision).sum::<f64>() / total_cases as f64;

        let avg_dependency_recall =
            results.iter().map(|r| r.dependency_recall).sum::<f64>() / total_cases as f64;

        let avg_dependency_f1 =
            results.iter().map(|r| r.dependency_f1).sum::<f64>() / total_cases as f64;

        let avg_resource_error =
            results.iter().map(|r| r.resource_error).sum::<f64>() / total_cases as f64;

        let overall_score =
            results.iter().map(|r| r.overall_score).sum::<f64>() / total_cases as f64;

        EvaluationMetrics {
            total_cases,
            passed_cases,
            avg_complexity_error,
            avg_category_precision,
            avg_category_recall,
            avg_category_f1,
            multi_step_accuracy,
            avg_dependency_precision,
            avg_dependency_recall,
            avg_dependency_f1,
            avg_resource_error,
            overall_score,
        }
    }

    pub fn generate_report(&self, metrics: &EvaluationMetrics) -> String {
        format!(
            r#"
=== LLM Evaluation Report ===

Overall Performance:
- Total Cases: {}
- Passed Cases: {} ({:.1}%)
- Overall Score: {:.3}

Complexity Analysis:
- Average Error: {:.3}

Category Classification:
- Precision: {:.3}
- Recall: {:.3}
- F1-Score: {:.3}

Multi-Step Detection:
- Accuracy: {:.1}%

Dependency Analysis:
- Precision: {:.3}
- Recall: {:.3}
- F1-Score: {:.3}

Resource Estimation:
- Average Error: {:.3}

Performance Summary:
{}
"#,
            metrics.total_cases,
            metrics.passed_cases,
            (metrics.passed_cases as f64 / metrics.total_cases as f64) * 100.0,
            metrics.overall_score,
            metrics.avg_complexity_error,
            metrics.avg_category_precision,
            metrics.avg_category_recall,
            metrics.avg_category_f1,
            metrics.multi_step_accuracy * 100.0,
            metrics.avg_dependency_precision,
            metrics.avg_dependency_recall,
            metrics.avg_dependency_f1,
            metrics.avg_resource_error,
            self.generate_performance_summary(metrics)
        )
    }

    fn generate_performance_summary(&self, metrics: &EvaluationMetrics) -> String {
        let score = metrics.overall_score;
        match score {
            s if s >= 0.9 => {
                "🟢 Excellent: The model performs exceptionally well across all metrics".to_string()
            },
            s if s >= 0.8 => {
                "🟡 Good: The model performs well with minor areas for improvement".to_string()
            },
            s if s >= 0.7 => {
                "🟠 Fair: The model shows decent performance but needs optimization".to_string()
            },
            s if s >= 0.6 => "🔴 Poor: The model requires significant improvement".to_string(),
            _ => "❌ Critical: The model performance is unacceptable and needs major fixes"
                .to_string(),
        }
    }

    pub async fn benchmark(
        &self,
        dataset_name: &str,
        iterations: usize,
    ) -> Result<Vec<EvaluationMetrics>> {
        let mut results = Vec::new();

        for i in 0..iterations {
            println!("Running benchmark iteration {} of {}", i + 1, iterations);
            let metrics = self.evaluate_dataset(dataset_name).await?;
            results.push(metrics);
        }

        Ok(results)
    }

    pub fn create_sample_dataset() -> EvaluationDataset {
        EvaluationDataset {
            name: "sample_evaluation".to_string(),
            version: "1.0.0".to_string(),
            description: "Sample evaluation dataset for UnifiedQueryAnalyzer".to_string(),
            cases: vec![
                EvaluationCase {
                    id: "simple_ping".to_string(),
                    query: "ping google.com".to_string(),
                    expected_complexity: 0.2,
                    expected_categories: vec!["network".to_string(), "diagnostics".to_string()],
                    expected_is_multi_step: false,
                    expected_dependencies: vec![],
                    expected_resource_estimate: 10.0,
                    description: "Simple network ping command".to_string(),
                },
                EvaluationCase {
                    id: "complex_deployment".to_string(),
                    query: "Deploy my React app to AWS with SSL certificate and monitoring"
                        .to_string(),
                    expected_complexity: 0.9,
                    expected_categories: vec![
                        "deployment".to_string(),
                        "cloud".to_string(),
                        "web".to_string(),
                    ],
                    expected_is_multi_step: true,
                    expected_dependencies: vec![
                        "AWS account".to_string(),
                        "SSL certificate".to_string(),
                    ],
                    expected_resource_estimate: 80.0,
                    description: "Complex multi-step deployment task".to_string(),
                },
                EvaluationCase {
                    id: "file_operation".to_string(),
                    query: "List all Python files in the project directory".to_string(),
                    expected_complexity: 0.3,
                    expected_categories: vec!["filesystem".to_string(), "search".to_string()],
                    expected_is_multi_step: false,
                    expected_dependencies: vec![],
                    expected_resource_estimate: 15.0,
                    description: "File system operation".to_string(),
                },
                EvaluationCase {
                    id: "data_processing".to_string(),
                    query: "Process CSV data, clean it, analyze trends, and generate \
                            visualizations"
                        .to_string(),
                    expected_complexity: 0.8,
                    expected_categories: vec![
                        "data".to_string(),
                        "analysis".to_string(),
                        "visualization".to_string(),
                    ],
                    expected_is_multi_step: true,
                    expected_dependencies: vec![
                        "CSV file".to_string(),
                        "data processing tools".to_string(),
                    ],
                    expected_resource_estimate: 70.0,
                    description: "Multi-step data processing pipeline".to_string(),
                },
            ],
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Arc;

    use tokio;

    use super::*;
    use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;

    struct MockLLMService;

    #[async_trait::async_trait]
    impl QueryAnalysisLLM for MockLLMService {
        async fn generate_analysis(&self, _prompt: &str) -> Result<SimplifiedLLMResponse> {
            Ok(SimplifiedLLMResponse::content_only(
                r#"{
                "complexity_score": 0.5,
                "categories": ["test"],
                "is_multi_step": true,
                "dependencies": ["mock_dependency"],
                "estimated_resources": 50.0,
                "reasoning": "Mock response for testing"
            }"#
                .to_string(),
            ))
        }

        async fn is_available(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_evaluation_metrics_calculation() {
        // Test that classification metrics are calculated correctly
        let evaluator = create_test_evaluator().await;

        let actual = vec!["cat1".to_string(), "cat2".to_string()];
        let expected = vec!["cat1".to_string(), "cat3".to_string()];

        let (precision, recall, f1) =
            evaluator.calculate_classification_metrics(&actual, &expected);

        // 1 intersection, 2 actual, 2 expected
        assert_eq!(precision, 0.5); // 1/2
        assert_eq!(recall, 0.5); // 1/2
        assert_eq!(f1, 0.5); // 2 * 0.5 * 0.5 / (0.5 + 0.5)
    }

    #[tokio::test]
    async fn test_sample_dataset_creation() {
        let dataset = LLMEvaluator::create_sample_dataset();

        assert_eq!(dataset.name, "sample_evaluation");
        assert_eq!(dataset.version, "1.0.0");
        assert_eq!(dataset.cases.len(), 4);

        // Verify first case
        let first_case = &dataset.cases[0];
        assert_eq!(first_case.id, "simple_ping");
        assert_eq!(first_case.query, "ping google.com");
        assert_eq!(first_case.expected_complexity, 0.2);
    }

    async fn create_test_evaluator() -> LLMEvaluator {
        use crate::magician_v2::prompts::{
            json_storage::JsonStorageConfig, JsonPromptStorage, PromptManager,
        };

        let llm_service: Arc<dyn QueryAnalysisLLM> = Arc::new(MockLLMService);

        // Create mock tool catalog for tests (no need for actual registry)
        struct MockToolCatalog;

        #[async_trait::async_trait]
        impl ToolCatalog for MockToolCatalog {
            async fn list_tool_names(&self, _context: &ExecutionContext) -> Vec<String> {
                vec!["test_tool".to_string()]
            }

            async fn available_categories(&self, _context: &ExecutionContext) -> Vec<String> {
                vec![
                    "test".to_string(),
                    "network".to_string(),
                    "diagnostics".to_string(),
                ]
            }

            async fn get_tool_metadata(
                &self,
                _tool_name: &str,
                _context: &ExecutionContext,
            ) -> Option<HashMap<String, serde_json::Value>> {
                None
            }

            async fn filtered_tools_by_categories(
                &self,
                _categories: &[String],
                _context: &ExecutionContext,
            ) -> anyhow::Result<Vec<runtime_core::ToolInfo>> {
                Ok(vec![])
            }

            async fn all_tools(
                &self,
                _context: &ExecutionContext,
            ) -> anyhow::Result<Vec<runtime_core::ToolInfo>> {
                Ok(vec![])
            }

            async fn category_tool_counts(
                &self,
                _context: &ExecutionContext,
            ) -> anyhow::Result<HashMap<String, usize>> {
                Ok(HashMap::new())
            }
        }

        let tool_catalog: Arc<dyn ToolCatalog> = Arc::new(MockToolCatalog);

        let data_dir = crate::magician_v2::prompts::json_storage::default_prompt_dir();
        let config = JsonStorageConfig {
            storage_dir: data_dir,
            enable_cache: true,
            max_cache_entries: 10,
        };
        let storage = JsonPromptStorage::new(config).unwrap();
        let prompt_manager = Arc::new(PromptManager::new(Arc::new(storage)));

        LLMEvaluator::new(llm_service, tool_catalog, prompt_manager)
    }
}
