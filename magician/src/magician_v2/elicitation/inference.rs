//! Parameter inference service implementation
//!
//! Infers parameter values using various methods (LLM, rules, defaults, auto-fill).

use crate::magician_v2::elicitation::{
    manager::ElicitationError,
    orchestrator_trait::ParameterInferenceService,
    types::{InferenceMethod, InferenceResult, InferenceSource, ParameterContext},
};
use crate::magician_v2::prompts::{
    constants::{names, versions},
    manager::PromptManager,
};
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};
use crate::magician_v2::strategy::plan::UnresolvedInput;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

const DEFAULT_AUTO_FILL_CONFIDENCE: f32 = 0.8;

/// Implementation of parameter inference service
///
/// Tries multiple inference methods in order of confidence:
/// 1. Auto-fill (if configured)
/// 2. Default value (if configured)
/// 3. LLM-based inference (using hints and context)
pub struct ParameterInferenceServiceImpl {
    /// LLM service for intelligent inference
    llm_service: Arc<OperationLlmRouter>,

    /// Prompt manager for template management
    prompt_manager: Arc<PromptManager>,
}

impl ParameterInferenceServiceImpl {
    /// Maximum number of retries for LLM calls
    const MAX_RETRIES: u32 = 2;

    /// Create a new inference service instance
    pub fn new(llm_service: Arc<OperationLlmRouter>, prompt_manager: Arc<PromptManager>) -> Self {
        info!("Creating ParameterInferenceService");
        Self {
            llm_service,
            prompt_manager,
        }
    }

    /// Call LLM with retry logic for transient failures
    async fn call_llm_with_retry(
        &self,
        operation: &LLMOperation,
        prompt: &str,
        param_name: &str,
        operation_type: &str,
    ) -> Result<serde_json::Value, ElicitationError> {
        let mut last_error: Option<String> = None;

        for attempt in 1..=Self::MAX_RETRIES {
            match self
                .llm_service
                .generate_for_operation(operation, prompt)
                .await
            {
                Ok(response) => {
                    match serde_json::from_str::<serde_json::Value>(&response.content) {
                        Ok(json) => {
                            if attempt > 1 {
                                info!(
                                    "[LLM-RETRY] ✅ {} succeeded for '{}' on attempt {}",
                                    operation_type, param_name, attempt
                                );
                            }
                            return Ok(json);
                        },
                        Err(e) => {
                            last_error = Some(format!("JSON parse error: {}", e));
                            warn!(
                                "[LLM-RETRY] {} for '{}' attempt {}/{}: Invalid JSON response: {}",
                                operation_type,
                                param_name,
                                attempt,
                                Self::MAX_RETRIES,
                                e
                            );

                            // On last attempt, return error
                            if attempt == Self::MAX_RETRIES {
                                return Err(ElicitationError::InvalidFormat(format!(
                                    "LLM returned invalid JSON after {} attempts: {}",
                                    Self::MAX_RETRIES,
                                    e
                                )));
                            }

                            // Otherwise, retry with slight delay
                            tokio::time::sleep(tokio::time::Duration::from_millis(
                                100 * attempt as u64,
                            ))
                            .await;
                        },
                    }
                },
                Err(e) => {
                    last_error = Some(format!("LLM service error: {}", e));
                    warn!(
                        "[LLM-RETRY] {} for '{}' attempt {}/{}: LLM service error: {}",
                        operation_type,
                        param_name,
                        attempt,
                        Self::MAX_RETRIES,
                        e
                    );

                    // On last attempt, return error
                    if attempt == Self::MAX_RETRIES {
                        return Err(ElicitationError::LlmServiceError(format!(
                            "LLM service failed after {} attempts: {}",
                            Self::MAX_RETRIES,
                            e
                        )));
                    }

                    // Otherwise, retry with exponential backoff
                    tokio::time::sleep(tokio::time::Duration::from_millis(200 * attempt as u64))
                        .await;
                },
            }
        }

        // Should never reach here due to return statements above, but for compiler
        Err(ElicitationError::LlmServiceError(
            last_error.unwrap_or_else(|| "Unknown error after retries".to_string()),
        ))
    }

    /// LLM Extraction: Extract explicit parameter values from user message
    ///
    /// Analyzes ONLY the current user message for explicitly mentioned values.
    /// High confidence (0.7-0.9) when value is clearly stated.
    async fn llm_extract_from_message(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<Option<InferenceResult>, ElicitationError> {
        debug!(
            "Attempting LLM extraction for parameter: {}",
            input.parameter
        );

        // Build variables for prompt rendering
        let mut variables = HashMap::new();
        variables.insert("parameter_name".to_string(), input.parameter.clone());
        variables.insert("display_name".to_string(), input.display_name.clone());
        variables.insert("description".to_string(), input.prompt.clone());
        variables.insert("user_message".to_string(), context.user_message.clone());

        // Get rendered prompt from PromptManager
        let prompt = self
            .prompt_manager
            .get_rendered_prompt(
                names::PARAM_EXTRACTION,
                versions::PARAM_EXTRACTION,
                variables,
            )
            .await
            .map_err(|e| {
                ElicitationError::LlmServiceError(format!(
                    "Failed to load param_extraction prompt: {}",
                    e
                ))
            })?;

        // Use retry logic for LLM calls with ParameterExtraction operation (nano)
        match self
            .call_llm_with_retry(
                &LLMOperation::ParameterExtraction,
                &prompt,
                &input.parameter,
                "LLM-EXTRACTION",
            )
            .await
        {
            Ok(json) => {
                let found = json["found"].as_bool().unwrap_or(false);

                if found {
                    let value = json["value"].clone();
                    let confidence = json["confidence"].as_f64().unwrap_or(0.75);
                    let reasoning = json["reasoning"]
                        .as_str()
                        .unwrap_or("LLM extraction")
                        .to_string();

                    info!(
                        "[LLM-EXTRACTION] ✅ Extracted '{}' = {:?} (confidence: {:.2})",
                        input.parameter, value, confidence
                    );

                    Ok(Some(InferenceResult {
                        value: Some(value),
                        confidence,
                        method: InferenceMethod::LLMBased,
                        explanation: format!("LLM extraction: {}", reasoning),
                        sources: vec![InferenceSource {
                            source_type: "llm_extraction".to_string(),
                            weight: confidence,
                            description: reasoning,
                        }],
                    }))
                } else {
                    debug!(
                        "[LLM-EXTRACTION] ⏭️ No explicit value found for '{}'",
                        input.parameter
                    );
                    Ok(None)
                }
            },
            Err(e) => {
                warn!(
                    "[LLM-EXTRACTION] Failed after retries for '{}': {}",
                    input.parameter, e
                );
                Ok(None)
            },
        }
    }

    /// LLM Context-Aware Defaults: Infer sensible defaults from context
    ///
    /// Uses LLM to determine appropriate default values based on:
    /// - Tool purpose and operation type
    /// - User intent and urgency
    /// - Already resolved parameters
    /// Medium confidence (0.6-0.8)
    async fn llm_infer_sensible_default(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<Option<InferenceResult>, ElicitationError> {
        debug!(
            "Attempting LLM default inference for parameter: {}",
            input.parameter
        );

        // Build context for LLM
        let mut context_info = String::new();

        if let Some(tool_meta) = &context.tool_context {
            context_info.push_str(&format!("Tool: {}\n", tool_meta.name));
            context_info.push_str(&format!("Tool Purpose: {}\n", tool_meta.description));
        }

        if !context.slot_context.is_empty() {
            context_info.push_str("\nAlready Known Parameters:\n");
            for (key, value) in &context.slot_context {
                context_info.push_str(&format!("- {}: {}\n", key, value));
            }
        }

        if !input.inference_hints.is_empty() {
            context_info.push_str("\nInference Hints:\n");
            for hint in &input.inference_hints {
                context_info.push_str(&format!("- {}\n", hint));
            }
        }

        // Build variables for prompt rendering
        let mut variables = HashMap::new();
        variables.insert("parameter_name".to_string(), input.parameter.clone());
        variables.insert("display_name".to_string(), input.display_name.clone());
        variables.insert("description".to_string(), input.prompt.clone());
        variables.insert("user_message".to_string(), context.user_message.clone());
        variables.insert("context_info".to_string(), context_info);

        // Get rendered prompt from PromptManager
        let prompt = self
            .prompt_manager
            .get_rendered_prompt(
                names::PARAM_DEFAULT_INFERENCE,
                versions::PARAM_DEFAULT_INFERENCE,
                variables,
            )
            .await
            .map_err(|e| {
                ElicitationError::LlmServiceError(format!(
                    "Failed to load param_default_inference prompt: {}",
                    e
                ))
            })?;

        // Use retry logic for LLM calls with ParameterDefaultInference operation (small)
        match self
            .call_llm_with_retry(
                &LLMOperation::ParameterDefaultInference,
                &prompt,
                &input.parameter,
                "LLM-DEFAULT",
            )
            .await
        {
            Ok(json) => {
                let can_infer = json["can_infer"].as_bool().unwrap_or(false);

                if can_infer {
                    let value = json["value"].clone();
                    let confidence = json["confidence"].as_f64().unwrap_or(0.7);
                    let reasoning = json["reasoning"]
                        .as_str()
                        .unwrap_or("LLM default inference")
                        .to_string();

                    info!(
                        "[LLM-DEFAULT] ✅ Inferred default for '{}' = {:?} (confidence: {:.2})",
                        input.parameter, value, confidence
                    );

                    Ok(Some(InferenceResult {
                        value: Some(value),
                        confidence,
                        method: InferenceMethod::LLMBased,
                        explanation: format!("LLM default: {}", reasoning),
                        sources: vec![InferenceSource {
                            source_type: "llm_default".to_string(),
                            weight: confidence,
                            description: reasoning,
                        }],
                    }))
                } else {
                    debug!(
                        "[LLM-DEFAULT] ⏭️ Cannot infer default for '{}'",
                        input.parameter
                    );
                    Ok(None)
                }
            },
            Err(e) => {
                warn!(
                    "[LLM-DEFAULT] Failed after retries for '{}': {}",
                    input.parameter, e
                );
                Ok(None)
            },
        }
    }
}

#[async_trait]
impl ParameterInferenceService for ParameterInferenceServiceImpl {
    async fn infer(
        &self,
        input: &UnresolvedInput,
        context: &ParameterContext,
    ) -> Result<InferenceResult, ElicitationError> {
        info!(
            "[INFERENCE] Attempting to infer parameter: {} (priority: {:?})",
            input.parameter, input.priority
        );

        // Method 1: Try auto_fill first (highest confidence - pre-configured)
        if let Some(auto_fill) = &input.auto_fill {
            let confidence = input
                .auto_fill_confidence
                .unwrap_or(DEFAULT_AUTO_FILL_CONFIDENCE);
            info!(
                "[INFERENCE] ✅ Using auto_fill for '{}' (confidence: {:.2})",
                input.parameter, confidence
            );
            return Ok(InferenceResult {
                value: Some(auto_fill.clone()),
                confidence: confidence as f64,
                method: InferenceMethod::AutoFill,
                explanation: "Used pre-configured auto-fill value".to_string(),
                sources: vec![InferenceSource {
                    source_type: "auto_fill".to_string(),
                    weight: confidence as f64,
                    description: "Pre-configured auto-fill".to_string(),
                }],
            });
        }

        // Method 2: Try LLM extraction (high confidence - explicit values)
        info!(
            "[INFERENCE] Trying LLM extraction for '{}'...",
            input.parameter
        );
        match self.llm_extract_from_message(input, context).await? {
            Some(result) => {
                info!(
                    "[INFERENCE] ✅ LLM extraction succeeded for '{}' (confidence: {:.2})",
                    input.parameter, result.confidence
                );
                return Ok(result);
            },
            None => {
                debug!(
                    "[INFERENCE] ⏭️ LLM extraction found no explicit value for '{}'",
                    input.parameter
                );
            },
        }

        // Method 3: Try LLM context-aware defaults (medium confidence)
        info!(
            "[INFERENCE] Trying LLM default inference for '{}'...",
            input.parameter
        );
        match self.llm_infer_sensible_default(input, context).await? {
            Some(result) => {
                info!(
                    "[INFERENCE] ✅ LLM default inference succeeded for '{}' (confidence: {:.2})",
                    input.parameter, result.confidence
                );
                return Ok(result);
            },
            None => {
                debug!(
                    "[INFERENCE] ⏭️ LLM cannot infer default for '{}'",
                    input.parameter
                );
            },
        }

        // Method 4: Try static default_value (medium-low confidence - fallback)
        if let Some(default_value) = &input.default_value {
            info!(
                "[INFERENCE] ✅ Using static default for '{}'",
                input.parameter
            );
            return Ok(InferenceResult {
                value: Some(default_value.clone()),
                confidence: 0.6,
                method: InferenceMethod::Default,
                explanation: "Used static default value from configuration".to_string(),
                sources: vec![InferenceSource {
                    source_type: "static_default".to_string(),
                    weight: 0.6,
                    description: "Static default from configuration".to_string(),
                }],
            });
        }

        // Method 5: No inference possible - must ask user
        info!(
            "[INFERENCE] ❌ No inference method succeeded for '{}' - must ask user",
            input.parameter
        );
        Ok(InferenceResult {
            value: None,
            confidence: 0.0,
            method: InferenceMethod::Default,
            explanation: "No inference method available - user input required".to_string(),
            sources: vec![],
        })
    }

    async fn can_infer(
        &self,
        input: &UnresolvedInput,
        _context: &ParameterContext,
    ) -> Result<bool, ElicitationError> {
        Ok(input.auto_fill.is_some()
            || input.default_value.is_some()
            || !input.inference_hints.is_empty())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::config::MagicianConfig;
    use crate::magician_v2::prompts::manager::PromptManager;
    use crate::magician_v2::strategy::plan::{
        AskTiming, DiscoveryTiming, InputSource, QuestionPriority,
    };
    use serde_json::json;
    use std::collections::HashMap;

    fn create_test_input(
        auto_fill: Option<serde_json::Value>,
        auto_fill_confidence: Option<f32>,
        default_value: Option<serde_json::Value>,
        inference_hints: Vec<String>,
    ) -> UnresolvedInput {
        UnresolvedInput {
            id: "test_param".to_string(),
            parameter: "test_param".to_string(),
            display_name: "Test Parameter".to_string(),
            step_id: Some("step_1".to_string()),
            linked_steps: vec!["step_1".to_string()],
            expected_type: Some("string".to_string()),
            json_schema: None,
            prompt: "Enter test parameter".to_string(),
            required: true,
            notes: None,
            priority: QuestionPriority::PreExecution,
            ask_timing: AskTiming::PreExecution,
            discovery_timing: DiscoveryTiming::PreExecution,
            default_value,
            inference_hints,
            inference_threshold: 0.7,
            auto_fill,
            auto_fill_confidence,
            source: InputSource::Planner,
            created_at: None,
            updated_at: None,
            status: None,
        }
    }

    fn create_test_context() -> ParameterContext {
        ParameterContext {
            execution_id: "test_thread".to_string(),
            user_message: "test message".to_string(),
            tool_context: None,
            slot_context: HashMap::new(),
            observations: vec![],
            stage: crate::magician_v2::elicitation::types::WorkflowStage::Planning,
            prompt_identity: None,
        }
    }

    async fn create_test_service() -> ParameterInferenceServiceImpl {
        // Create minimal config for testing
        let config = MagicianConfig::default();
        let llm_service = Arc::new(OperationLlmRouter::new(config.router_config().cloned()));

        // Create test prompt storage
        let storage_config = crate::magician_v2::prompts::json_storage::JsonStorageConfig {
            storage_dir: crate::magician_v2::prompts::json_storage::default_prompt_dir(),
            enable_cache: false,
            max_cache_entries: 100,
        };
        let storage = crate::magician_v2::prompts::JsonPromptStorage::new(storage_config).unwrap();
        let prompt_manager = Arc::new(PromptManager::new(Arc::new(storage)));

        ParameterInferenceServiceImpl::new(llm_service, prompt_manager)
    }

    fn assert_close(expected: f64, actual: f64) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "expected {expected}, got {actual}"
        );
    }

    #[tokio::test]
    async fn test_auto_fill_inference() {
        let service = create_test_service().await;

        // Test with auto_fill value and explicit confidence
        let input = create_test_input(Some(json!("auto_filled_value")), Some(0.9), None, vec![]);
        let context = create_test_context();

        let result = service.infer(&input, &context).await;
        assert!(result.is_ok());

        let inference_result = result.unwrap();
        assert!(inference_result.value.is_some());
        assert_eq!(inference_result.value.unwrap(), json!("auto_filled_value"));
        assert_close(0.9, inference_result.confidence);
        assert_eq!(inference_result.method, InferenceMethod::AutoFill);
        assert_eq!(inference_result.sources.len(), 1);
        assert_eq!(inference_result.sources[0].source_type, "auto_fill");

        // Test with auto_fill but no explicit confidence (should default to constant)
        let input2 = create_test_input(Some(json!({"key": "value"})), None, None, vec![]);

        let result2 = service.infer(&input2, &context).await;
        assert!(result2.is_ok());

        let inference_result2 = result2.unwrap();
        assert!(inference_result2.value.is_some());
        assert_close(
            DEFAULT_AUTO_FILL_CONFIDENCE as f64,
            inference_result2.confidence,
        ); // Default confidence
        assert_eq!(inference_result2.method, InferenceMethod::AutoFill);
    }

    #[tokio::test]
    async fn test_default_value_inference() {
        let service = create_test_service().await;

        // Test with default value (no auto_fill, so should use default)
        let input = create_test_input(None, None, Some(json!("default_value")), vec![]);
        let context = create_test_context();

        let result = service.infer(&input, &context).await;
        assert!(result.is_ok());

        let inference_result = result.unwrap();
        assert!(inference_result.value.is_some());
        assert_eq!(inference_result.value.unwrap(), json!("default_value"));
        assert_close(0.6, inference_result.confidence); // Default value confidence
        assert_eq!(inference_result.method, InferenceMethod::Default);
        assert_eq!(inference_result.sources.len(), 1);
        assert_eq!(inference_result.sources[0].source_type, "static_default");
        assert_eq!(inference_result.sources[0].weight, 0.6);

        // Test that auto_fill takes precedence over default
        let input2 = create_test_input(
            Some(json!("auto_filled_value")),
            Some(0.85),
            Some(json!("default_value")),
            vec![],
        );

        let result2 = service.infer(&input2, &context).await;
        assert!(result2.is_ok());

        let inference_result2 = result2.unwrap();
        assert_eq!(inference_result2.value.unwrap(), json!("auto_filled_value"));
        assert_eq!(inference_result2.method, InferenceMethod::AutoFill); // Auto-fill wins
    }

    #[tokio::test]
    #[ignore] // Requires actual LLM service configuration with API keys
    async fn test_llm_inference() {
        // This test requires a real LLM service with proper API configuration.
        // To enable this test:
        // 1. Set up OpenAI/Anthropic/Ollama API keys in config
        // 2. Ensure magician-config.yaml has proper LLM configuration
        // 3. Run with: cargo test test_llm_inference -- --ignored --nocapture

        let service = create_test_service().await;

        // Test with inference hints but no auto_fill or default
        let input = create_test_input(
            None,
            None,
            None,
            vec!["user_location".to_string(), "timezone".to_string()],
        );
        let context = create_test_context();

        let result = service.infer(&input, &context).await;
        assert!(result.is_ok());

        let inference_result = result.unwrap();
        // LLM inference should either return a value with 0.75 confidence
        // or CANNOT_INFER with 0.0 confidence
        if inference_result.value.is_some() {
            assert_close(0.75, inference_result.confidence);
            assert_eq!(inference_result.method, InferenceMethod::LLMBased);
        } else {
            assert_eq!(inference_result.confidence, 0.0);
        }
    }

    #[tokio::test]
    async fn test_no_inference_available() {
        let service = create_test_service().await;

        // Test with no inference methods available (no auto_fill, no default, no hints)
        let input = create_test_input(None, None, None, vec![]);
        let context = create_test_context();

        let result = service.infer(&input, &context).await;
        assert!(result.is_ok());

        let inference_result = result.unwrap();
        assert!(inference_result.value.is_none());
        assert_eq!(inference_result.confidence, 0.0);
        assert_eq!(inference_result.method, InferenceMethod::Default);
        assert!(inference_result
            .explanation
            .contains("No inference method available"));
        assert_eq!(inference_result.sources.len(), 0);
    }

    #[tokio::test]
    async fn test_can_infer() {
        let service = create_test_service().await;
        let context = create_test_context();

        // Can infer with auto_fill
        let input1 = create_test_input(Some(json!("value")), None, None, vec![]);
        assert!(service.can_infer(&input1, &context).await.unwrap());

        // Can infer with default_value
        let input2 = create_test_input(None, None, Some(json!("value")), vec![]);
        assert!(service.can_infer(&input2, &context).await.unwrap());

        // Can infer with inference_hints
        let input3 = create_test_input(None, None, None, vec!["hint".to_string()]);
        assert!(service.can_infer(&input3, &context).await.unwrap());

        // Cannot infer with nothing
        let input4 = create_test_input(None, None, None, vec![]);
        assert!(!service.can_infer(&input4, &context).await.unwrap());
    }
}
