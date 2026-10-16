//! LLM-based input interpretation for agentic execution.
//!
//! This module handles the interpretation of user input responses during
//! the pause/resume cycle. It uses a lightweight LLM (nano model) to validate
//! and extract values from user responses, handling natural language and
//! edge cases gracefully.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

use super::PendingInput;
use crate::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use crate::magician_v2::execution::PromptIdentityContext;
use crate::magician_v2::prompt_identity::render_prompt_identity_section;
use crate::magician_v2::prompts::{constants, PromptManager};
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};

// ============================================================================
// Interpretation Result Types
// ============================================================================

/// Result of interpreting user input for a pending input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretationResult {
    /// The pending input ID this interpretation is for
    pub input_id: String,

    /// Whether the interpretation was successful
    pub success: bool,

    /// The extracted/validated value (None if interpretation failed)
    pub value: Option<Value>,

    /// Confidence in the interpretation (0.0 to 1.0)
    pub confidence: f64,

    /// Human-readable explanation of the interpretation
    pub explanation: String,

    /// Any issues or warnings (e.g., ambiguous input, partial match)
    pub warnings: Vec<String>,
}

impl InterpretationResult {
    /// Create a successful interpretation result
    pub fn success(input_id: String, value: Value, confidence: f64, explanation: String) -> Self {
        Self {
            input_id,
            success: true,
            value: Some(value),
            confidence,
            explanation,
            warnings: Vec::new(),
        }
    }

    /// Create a failed interpretation result
    pub fn failure(input_id: String, explanation: String) -> Self {
        Self {
            input_id,
            success: false,
            value: None,
            confidence: 0.0,
            explanation,
            warnings: Vec::new(),
        }
    }

    /// Add a warning to the result
    pub fn with_warning(mut self, warning: String) -> Self {
        self.warnings.push(warning);
        self
    }
}

/// Raw LLM response for input interpretation.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawInterpretation {
    /// Whether the input is valid for the parameter
    valid: bool,

    /// The extracted value (in appropriate type)
    #[serde(default)]
    value: Option<Value>,

    /// Confidence score (0.0 to 1.0)
    #[serde(default = "default_confidence")]
    confidence: f64,

    /// Explanation of interpretation
    #[serde(default)]
    explanation: String,

    /// Any issues or warnings
    #[serde(default)]
    warnings: Vec<String>,
}

fn default_confidence() -> f64 {
    0.5
}

// ============================================================================
// Interpreter Implementation
// ============================================================================

/// Interprets user input responses for pending inputs using LLM.
pub struct AgenticInputInterpreter {
    llm_service: Arc<OperationLlmRouter>,
    prompt_manager: Arc<PromptManager>,
    prompt_identity: Option<PromptIdentityContext>,
    llm_telemetry: Option<(OperationLlmTelemetryContext, OperationLlmCallAttribution)>,
}

impl AgenticInputInterpreter {
    /// Create a new interpreter with the given LLM service and prompt manager.
    pub fn new(llm_service: Arc<OperationLlmRouter>, prompt_manager: Arc<PromptManager>) -> Self {
        Self {
            llm_service,
            prompt_manager,
            prompt_identity: None,
            llm_telemetry: None,
        }
    }

    /// Attach prompt identity context for safe system-prompt personality injection.
    pub fn with_prompt_identity(mut self, prompt_identity: Option<PromptIdentityContext>) -> Self {
        self.prompt_identity = prompt_identity;
        self
    }

    /// Attach scoped accounting for this direct operation-router call.
    pub fn with_llm_telemetry(
        mut self,
        context: OperationLlmTelemetryContext,
        attribution: OperationLlmCallAttribution,
    ) -> Self {
        self.llm_service = Arc::new(self.llm_service.with_scope_context(Some(context.scope())));
        self.llm_telemetry = Some((context, attribution));
        self
    }

    /// Interpret a user response for a specific pending input.
    ///
    /// # Arguments
    /// * `pending_input` - The pending input definition
    /// * `user_response` - The raw user response string
    /// * `context` - Optional additional context (e.g., current page state)
    ///
    /// # Returns
    /// An interpretation result with the extracted value or error details.
    pub async fn interpret(
        &self,
        pending_input: &PendingInput,
        user_response: &str,
        context: Option<&str>,
    ) -> Result<InterpretationResult> {
        info!(
            input_id = %pending_input.id,
            parameter = %pending_input.parameter,
            response_len = user_response.len(),
            "[AGENTIC-INPUT] Interpreting user response"
        );

        // Build the prompt
        let prompt = self
            .build_interpretation_prompt(pending_input, user_response, context)
            .await?;

        let mut system_variables = HashMap::new();
        system_variables.insert(
            "identity_section".to_string(),
            render_prompt_identity_section(self.prompt_identity.as_ref(), false),
        );
        let system_prompt = self
            .prompt_manager
            .get_rendered_prompt(
                constants::names::AGENTIC_INPUT_INTERPRETATION_SYSTEM,
                constants::versions::AGENTIC_INPUT_INTERPRETATION_SYSTEM,
                system_variables,
            )
            .await?;

        // Call LLM for interpretation using the AgenticInputInterpretation operation
        let started_at = std::time::Instant::now();
        let response = self
            .llm_service
            .generate_for_operation_with_system(
                &LLMOperation::AgenticInputInterpretation,
                Some(system_prompt.as_str()),
                &prompt,
            )
            .await?;

        let parsed = self.parse_interpretation_response(&response.content, &pending_input.id);
        if let Some((telemetry, attribution)) = self.llm_telemetry.as_ref() {
            let latency_ms = started_at.elapsed().as_millis() as u64;
            match parsed.as_ref() {
                Ok(_) => telemetry.emit_validated_success(
                    LLMOperation::AgenticInputInterpretation.as_str(),
                    &response,
                    latency_ms,
                    attribution.clone(),
                    "agentic_input_json",
                ),
                Err(error) => telemetry.emit_validation_failure(
                    LLMOperation::AgenticInputInterpretation.as_str(),
                    &response,
                    latency_ms,
                    attribution.clone(),
                    "agentic_input_json",
                    &error.to_string(),
                ),
            }
        }

        // Parse the response
        match parsed {
            Ok(result) => {
                info!(
                    input_id = %pending_input.id,
                    success = result.success,
                    confidence = result.confidence,
                    "[AGENTIC-INPUT] Interpretation complete"
                );
                Ok(result)
            },
            Err(e) => {
                warn!(
                    input_id = %pending_input.id,
                    error = %e,
                    "[AGENTIC-INPUT] Failed to parse interpretation response"
                );
                Ok(InterpretationResult::failure(
                    pending_input.id.clone(),
                    format!("Failed to interpret response: {}", e),
                ))
            },
        }
    }

    /// Interpret multiple pending inputs from a single user response.
    ///
    /// This is useful when the user provides multiple values in one response.
    pub async fn interpret_batch(
        &self,
        pending_inputs: &[PendingInput],
        user_response: &str,
        context: Option<&str>,
    ) -> Result<Vec<InterpretationResult>> {
        let mut results = Vec::new();

        // For now, interpret each input individually
        // Future optimization: batch LLM call for all inputs
        for input in pending_inputs {
            match self.interpret(input, user_response, context).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    results.push(InterpretationResult::failure(
                        input.id.clone(),
                        format!("Interpretation error: {}", e),
                    ));
                },
            }
        }

        Ok(results)
    }

    /// Build the interpretation prompt for the LLM.
    ///
    /// # Errors
    /// Returns an error if the prompt cannot be loaded from storage.
    /// No fallback is used - prompts must be properly configured.
    async fn build_interpretation_prompt(
        &self,
        pending_input: &PendingInput,
        user_response: &str,
        context: Option<&str>,
    ) -> Result<String> {
        let prompt_name = constants::names::AGENTIC_INPUT_INTERPRETATION;
        let prompt_version = constants::versions::AGENTIC_INPUT_INTERPRETATION;

        let mut variables = HashMap::new();
        variables.insert(
            "parameter_name".to_string(),
            pending_input.parameter.clone(),
        );
        variables.insert(
            "parameter_description".to_string(),
            pending_input.description.clone().unwrap_or_default(),
        );
        variables.insert("user_response".to_string(), user_response.to_string());
        variables.insert(
            "context".to_string(),
            context.unwrap_or("No additional context").to_string(),
        );

        // Get rendered prompt from storage - fail fast if not found
        self.prompt_manager
            .get_rendered_prompt(prompt_name, prompt_version, variables)
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to load prompt '{}' v{}: {}. Ensure the prompt file exists in data/magician_v2/prompts/",
                    prompt_name,
                    prompt_version,
                    e
                )
            })
    }

    /// Parse the LLM response into an interpretation result.
    fn parse_interpretation_response(
        &self,
        response: &str,
        input_id: &str,
    ) -> Result<InterpretationResult> {
        // Try to extract JSON from the response
        let json_str = self.extract_json(response)?;

        let raw: RawInterpretation = serde_json::from_str(&json_str)
            .map_err(|e| anyhow!("Failed to parse interpretation JSON: {}", e))?;

        let result = if raw.valid {
            InterpretationResult {
                input_id: input_id.to_string(),
                success: true,
                value: raw.value,
                confidence: raw.confidence,
                explanation: raw.explanation,
                warnings: raw.warnings,
            }
        } else {
            InterpretationResult {
                input_id: input_id.to_string(),
                success: false,
                value: None,
                confidence: raw.confidence,
                explanation: raw.explanation,
                warnings: raw.warnings,
            }
        };

        Ok(result)
    }

    /// Extract JSON from an LLM response that might have extra text.
    fn extract_json(&self, response: &str) -> Result<String> {
        // First try: maybe it's already valid JSON
        if serde_json::from_str::<serde_json::Value>(response.trim()).is_ok() {
            return Ok(response.trim().to_string());
        }

        // Second try: look for JSON in code blocks
        if let Some(start) = response.find("```json") {
            if let Some(end) = response[start..].find("```\n") {
                let json = &response[start + 7..start + end];
                if serde_json::from_str::<serde_json::Value>(json.trim()).is_ok() {
                    return Ok(json.trim().to_string());
                }
            }
        }

        // Third try: find first { and last }
        if let (Some(start), Some(end)) = (response.find('{'), response.rfind('}')) {
            if end > start {
                let json = &response[start..=end];
                if serde_json::from_str::<serde_json::Value>(json).is_ok() {
                    return Ok(json.to_string());
                }
            }
        }

        Err(anyhow!("Could not extract valid JSON from response"))
    }
}

// ============================================================================
// Simple Interpretations (no LLM needed)
// ============================================================================

/// Simple value extraction for common patterns (avoids LLM call).
pub fn try_simple_extraction(
    pending_input: &PendingInput,
    user_response: &str,
) -> Option<InterpretationResult> {
    let response = user_response.trim();

    // Handle common affirmative/negative patterns
    let lower = response.to_lowercase();
    if matches!(
        lower.as_str(),
        "yes" | "y" | "ok" | "okay" | "sure" | "confirm" | "proceed" | "true" | "1"
    ) {
        return Some(InterpretationResult::success(
            pending_input.id.clone(),
            Value::Bool(true),
            0.95,
            "Interpreted as confirmation/yes".to_string(),
        ));
    }

    if matches!(
        lower.as_str(),
        "no" | "n" | "cancel" | "stop" | "false" | "0" | "nope"
    ) {
        return Some(InterpretationResult::success(
            pending_input.id.clone(),
            Value::Bool(false),
            0.95,
            "Interpreted as rejection/no".to_string(),
        ));
    }

    // If it's a single word/line and looks like a direct answer, use it as-is
    if !response.contains('\n') && response.len() < 200 && !response.contains(' ') {
        // Likely a direct value (email, username, number, etc.)
        // Try to parse as number first
        if let Ok(num) = response.parse::<i64>() {
            return Some(InterpretationResult::success(
                pending_input.id.clone(),
                Value::Number(num.into()),
                0.9,
                "Interpreted as integer".to_string(),
            ));
        }
        if let Ok(num) = response.parse::<f64>() {
            return Some(InterpretationResult::success(
                pending_input.id.clone(),
                serde_json::Number::from_f64(num)
                    .map(Value::Number)
                    .unwrap_or(Value::String(response.to_string())),
                0.9,
                "Interpreted as number".to_string(),
            ));
        }

        // Use as string
        return Some(InterpretationResult::success(
            pending_input.id.clone(),
            Value::String(response.to_string()),
            0.85,
            "Interpreted as direct text value".to_string(),
        ));
    }

    // Complex response - needs LLM interpretation
    None
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_simple_extraction_yes() {
        let input = PendingInput::from_planning(
            "test_id",
            "confirm",
            None,
            Some("Confirm action?".to_string()),
        );

        let result = try_simple_extraction(&input, "yes").unwrap();
        assert!(result.success);
        assert_eq!(result.value, Some(Value::Bool(true)));
        assert!(result.confidence > 0.9);
    }

    #[test]
    fn test_simple_extraction_no() {
        let input = PendingInput::from_planning(
            "test_id",
            "confirm",
            None,
            Some("Confirm action?".to_string()),
        );

        let result = try_simple_extraction(&input, "no").unwrap();
        assert!(result.success);
        assert_eq!(result.value, Some(Value::Bool(false)));
    }

    #[test]
    fn test_simple_extraction_number() {
        let input = PendingInput::from_planning(
            "test_id",
            "count",
            None,
            Some("How many items?".to_string()),
        );

        let result = try_simple_extraction(&input, "42").unwrap();
        assert!(result.success);
        assert_eq!(result.value, Some(Value::Number(42.into())));
    }

    #[test]
    fn test_simple_extraction_text() {
        let input =
            PendingInput::from_planning("test_id", "email", None, Some("Enter email".to_string()));

        let result = try_simple_extraction(&input, "test@example.com").unwrap();
        assert!(result.success);
        assert_eq!(
            result.value,
            Some(Value::String("test@example.com".to_string()))
        );
    }

    #[test]
    fn test_simple_extraction_complex_needs_llm() {
        let input =
            PendingInput::from_planning("test_id", "notes", None, Some("Any notes?".to_string()));

        // Complex response with multiple lines should return None to trigger LLM
        let result = try_simple_extraction(&input, "Here are my notes:\n- Item 1\n- Item 2");
        assert!(result.is_none());
    }
}
