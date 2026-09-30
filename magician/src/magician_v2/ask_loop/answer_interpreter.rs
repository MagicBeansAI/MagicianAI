//! Answer Interpretation Service
//!
//! Provides LLM-based interpretation of user answers to clarification questions.
//! Distinguishes between direct answers, corrections, clarifications, and rejections.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, error, info, warn};

use crate::magician_v2::{
    analytics::operation_llm_telemetry::OperationLlmTelemetryScope,
    execution::PromptIdentityContext,
    prompt_identity::render_prompt_identity_section,
    prompts::{constants, PromptManager},
    slot_graph::SlotRecord,
};

/// Result of interpreting a user's answer to a clarification question
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretedAnswer {
    /// Type of answer provided
    pub answer_type: AnswerType,

    /// Extracted slot values (if any)
    pub slots: Vec<SlotRecord>,

    /// If answer_type is Correction, what needs to be corrected
    pub correction_target: Option<String>,

    /// If answer_type is Correction, the new value
    pub correction_value: Option<String>,

    /// If answer_type is Clarification, additional context provided
    pub clarification_text: Option<String>,

    /// If answer_type is Rejection, the reason given
    pub rejection_reason: Option<String>,

    /// Confidence in the interpretation (0.0 to 1.0)
    pub confidence: f64,

    /// Whether this answer suggests replanning is needed
    pub requires_replan: bool,

    /// Raw reasoning from LLM for debugging
    pub reasoning: String,
}

/// Classification of user answer types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnswerType {
    /// Direct answer providing requested slot value(s)
    DirectAnswer,

    /// Correction modifying previous information
    /// Example: "no, use WhatsApp instead"
    Correction,

    /// Clarification providing additional context
    /// Example: "it should support video calls"
    Clarification,

    /// Rejection declining to provide information
    /// Example: "I don't want to specify that now"
    Rejection,

    /// Multiple slot values in one response
    /// Example: "use Slack for team chat and email for clients"
    MultiValue,

    /// Ambiguous or unclear response
    Ambiguous,
}

/// Service for interpreting user answers using LLM
pub struct AnswerInterpreter<L> {
    llm_service: L,
    prompt_manager: Arc<PromptManager>,
}

impl<L> AnswerInterpreter<L>
where
    L: AnswerInterpretationLLM,
{
    /// Create a new answer interpreter
    pub fn new(llm_service: L, prompt_manager: Arc<PromptManager>) -> Self {
        Self {
            llm_service,
            prompt_manager,
        }
    }

    /// Interpret a user's answer to a clarification question
    ///
    /// # Arguments
    /// * `question_text` - The original question asked
    /// * `user_answer` - The user's response
    /// * `context` - Optional context about what information was being sought
    pub async fn interpret_answer(
        &self,
        question_text: &str,
        user_answer: &str,
        context: Option<&str>,
    ) -> Result<InterpretedAnswer, InterpreterError> {
        self.interpret_answer_with_identity(question_text, user_answer, context, None)
            .await
    }

    /// Interpret a user's answer with optional prompt identity context.
    pub async fn interpret_answer_with_identity(
        &self,
        question_text: &str,
        user_answer: &str,
        context: Option<&str>,
        prompt_identity: Option<&PromptIdentityContext>,
    ) -> Result<InterpretedAnswer, InterpreterError> {
        self.interpret_answer_with_identity_and_telemetry(
            question_text,
            user_answer,
            context,
            prompt_identity,
            None,
        )
        .await
    }

    pub async fn interpret_answer_with_identity_and_telemetry(
        &self,
        question_text: &str,
        user_answer: &str,
        context: Option<&str>,
        prompt_identity: Option<&PromptIdentityContext>,
        telemetry_scope: Option<&OperationLlmTelemetryScope>,
    ) -> Result<InterpretedAnswer, InterpreterError> {
        info!(
            "[MAGICIAN-ANSWER-INTERPRETER] === Starting answer interpretation ===\nQuestion: '{}'\nUser Answer: '{}'\nContext: {:?}",
            question_text, user_answer, context
        );

        // Load the answer interpretation prompt using constants
        info!(
            "[MAGICIAN-ANSWER-INTERPRETER] Loading prompt: name='{}', version='{}'",
            constants::names::ANSWER_INTERPRETATION,
            constants::versions::ANSWER_INTERPRETATION
        );

        let prompt_template = self
            .prompt_manager
            .get_prompt(
                constants::names::ANSWER_INTERPRETATION,
                constants::versions::ANSWER_INTERPRETATION,
            )
            .await
            .map_err(|e| {
                error!(
                    "[MAGICIAN-ANSWER-INTERPRETER] ❌ Failed to load prompt: {}",
                    e
                );
                InterpreterError::PromptLoadFailed(e.to_string())
            })?;

        info!("[MAGICIAN-ANSWER-INTERPRETER] ✓ Prompt loaded successfully");

        // Build the prompt with question and answer
        let prompt = self.build_interpretation_prompt(
            &prompt_template.content,
            question_text,
            user_answer,
            context,
        )?;

        let mut system_variables = std::collections::HashMap::new();
        system_variables.insert(
            "identity_section".to_string(),
            render_prompt_identity_section(prompt_identity, false),
        );
        let system_prompt = self
            .prompt_manager
            .get_rendered_prompt(
                constants::names::ANSWER_INTERPRETATION_SYSTEM,
                constants::versions::ANSWER_INTERPRETATION_SYSTEM,
                system_variables,
            )
            .await
            .map_err(|e| {
                error!(
                    "[MAGICIAN-ANSWER-INTERPRETER] ❌ Failed to load system prompt: {}",
                    e
                );
                InterpreterError::PromptLoadFailed(e.to_string())
            })?;

        info!(
            "[MAGICIAN-ANSWER-INTERPRETER] Calling LLM for answer interpretation\nPrompt length: {} chars",
            prompt.len()
        );
        debug!("[MAGICIAN-ANSWER-INTERPRETER] Full prompt:\n{}", prompt);

        // Call LLM for interpretation
        let llm_response = self
            .llm_service
            .interpret_with_system_and_telemetry(
                Some(system_prompt.as_str()),
                &prompt,
                telemetry_scope,
            )
            .await
            .map_err(|e| {
                error!("[MAGICIAN-ANSWER-INTERPRETER] LLM call failed: {}", e);
                InterpreterError::LLMFailed(e.to_string())
            })?;

        info!(
            "[MAGICIAN-ANSWER-INTERPRETER] LLM response received, length: {} chars",
            llm_response.len()
        );
        debug!(
            "[MAGICIAN-ANSWER-INTERPRETER] LLM response:\n{}",
            llm_response
        );

        // Parse LLM response
        let interpreted = self.parse_llm_response(&llm_response)?;

        info!(
            "[MAGICIAN-ANSWER-INTERPRETER] ✓ Interpreted as {:?} with confidence {:.2}, requires_replan={}",
            interpreted.answer_type, interpreted.confidence, interpreted.requires_replan
        );

        if interpreted.requires_replan {
            warn!("[MAGICIAN-ANSWER-INTERPRETER] ⚠ Answer requires replanning!");
        }

        Ok(interpreted)
    }

    /// Build the prompt for LLM interpretation
    fn build_interpretation_prompt(
        &self,
        template: &str,
        question_text: &str,
        user_answer: &str,
        context: Option<&str>,
    ) -> Result<String, InterpreterError> {
        let context_str = context.unwrap_or("(no additional context)");

        let prompt = template
            .replace("{question}", question_text)
            .replace("{answer}", user_answer)
            .replace("{context}", context_str);

        Ok(prompt)
    }

    /// Parse the LLM's interpretation response
    fn parse_llm_response(&self, response: &str) -> Result<InterpretedAnswer, InterpreterError> {
        // Try to parse as JSON first
        if let Ok(interpreted) = serde_json::from_str::<InterpretedAnswer>(response) {
            return Ok(interpreted);
        }

        // If JSON parsing fails, try to extract structured data from text
        // This is a fallback for LLMs that don't return perfect JSON
        self.parse_text_response(response)
    }

    /// Parse text response when JSON parsing fails
    ///
    /// SAFETY: This is a fallback that returns safe defaults instead of guessing
    /// intent from keyword matching. It sets low confidence to trigger re-analysis.
    fn parse_text_response(&self, response: &str) -> Result<InterpretedAnswer, InterpreterError> {
        warn!(
            "[ANSWER-INTERPRETER] JSON parsing failed. Using safe fallback defaults. \
             Response will be treated as direct answer with low confidence. \
             Original response: {}",
            response
        );

        // SAFE DEFAULTS:
        // - Assume DirectAnswer (safest assumption)
        // - Set low confidence (0.3) to trigger re-analysis or follow-up questions
        // - Do NOT trigger replanning (avoid expensive operations on uncertain data)
        // - Preserve original response text for manual review

        // Try to extract confidence if LLM included it in unstructured text
        let confidence = extract_confidence(response).unwrap_or(0.3); // Default to low confidence

        // Cap confidence at 0.4 for fallback parsing to ensure downstream systems
        // treat this as unreliable and ask for clarification if needed
        let confidence = confidence.min(0.4);

        Ok(InterpretedAnswer {
            answer_type: AnswerType::DirectAnswer,
            slots: Vec::new(), // Will be filled by clarifier if confidence is acceptable
            correction_target: None,
            correction_value: None,
            clarification_text: Some(format!(
                "I had trouble understanding that response. Could you rephrase? \
                 (Original: {})",
                response.chars().take(100).collect::<String>()
            )),
            rejection_reason: None,
            confidence,
            requires_replan: false, // Never trigger replanning on uncertain parsing
            reasoning: format!(
                "FALLBACK PARSING: LLM response was not valid JSON. \
                 Defaulting to DirectAnswer with low confidence. \
                 Original response: {}",
                response
            ),
        })
    }
}

/// Extract confidence value from text response
fn extract_confidence(text: &str) -> Option<f64> {
    // Look for patterns like "confidence: 0.8" or "confidence = 0.8"
    let text_lower = text.to_lowercase();
    if let Some(pos) = text_lower.find("confidence") {
        let after = &text[pos..];
        // Try to extract number after "confidence"
        for token in after.split_whitespace().skip(1).take(3) {
            if let Ok(value) = token
                .trim_matches(|c: char| !c.is_numeric() && c != '.')
                .parse::<f64>()
            {
                if (0.0..=1.0).contains(&value) {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Trait for LLM services that can interpret answers
#[async_trait::async_trait]
pub trait AnswerInterpretationLLM: Send + Sync {
    /// Interpret a user answer using LLM
    async fn interpret(&self, prompt: &str) -> Result<String, String>;

    /// Interpret a user answer with optional system prompt.
    ///
    /// Default implementation preserves backward compatibility by ignoring
    /// `system_prompt` and delegating to `interpret`.
    async fn interpret_with_system(
        &self,
        system_prompt: Option<&str>,
        prompt: &str,
    ) -> Result<String, String> {
        let _ = system_prompt;
        self.interpret(prompt).await
    }

    async fn interpret_with_system_and_telemetry(
        &self,
        system_prompt: Option<&str>,
        prompt: &str,
        telemetry_scope: Option<&OperationLlmTelemetryScope>,
    ) -> Result<String, String> {
        let _ = telemetry_scope;
        self.interpret_with_system(system_prompt, prompt).await
    }
}

/// Blanket implementation for Arc-wrapped trait objects
/// This allows using Arc<dyn AnswerInterpretationLLM> as the generic type L
#[async_trait::async_trait]
impl AnswerInterpretationLLM for Arc<dyn AnswerInterpretationLLM> {
    async fn interpret(&self, prompt: &str) -> Result<String, String> {
        (**self).interpret(prompt).await
    }

    async fn interpret_with_system(
        &self,
        system_prompt: Option<&str>,
        prompt: &str,
    ) -> Result<String, String> {
        (**self).interpret_with_system(system_prompt, prompt).await
    }

    async fn interpret_with_system_and_telemetry(
        &self,
        system_prompt: Option<&str>,
        prompt: &str,
        telemetry_scope: Option<&OperationLlmTelemetryScope>,
    ) -> Result<String, String> {
        (**self)
            .interpret_with_system_and_telemetry(system_prompt, prompt, telemetry_scope)
            .await
    }
}

/// Error types for answer interpretation
#[derive(Debug, Error)]
pub enum InterpreterError {
    #[error("failed to load prompt: {0}")]
    PromptLoadFailed(String),

    #[error("LLM call failed: {0}")]
    LLMFailed(String),

    #[error("failed to parse LLM response: {0}")]
    ParseFailed(String),

    #[error("invalid interpretation result: {0}")]
    InvalidResult(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::{PromptAgentKind, PromptIdentityContext};
    use crate::magician_v2::prompts::{Prompt, PromptCategory, PromptStore};

    struct MockLLM;

    #[async_trait::async_trait]
    impl AnswerInterpretationLLM for MockLLM {
        async fn interpret(&self, _prompt: &str) -> Result<String, String> {
            Ok(r#"{
                "answer_type": "DirectAnswer",
                "slots": [],
                "correction_target": null,
                "correction_value": null,
                "clarification_text": null,
                "rejection_reason": null,
                "confidence": 0.9,
                "requires_replan": false,
                "reasoning": "User provided a direct answer"
            }"#
            .to_string())
        }
    }

    struct SystemOnlyLLM {
        seen_system_prompts: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl SystemOnlyLLM {
        fn new() -> (Self, Arc<std::sync::Mutex<Vec<String>>>) {
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                Self {
                    seen_system_prompts: Arc::clone(&seen),
                },
                seen,
            )
        }
    }

    #[async_trait::async_trait]
    impl AnswerInterpretationLLM for SystemOnlyLLM {
        async fn interpret(&self, _prompt: &str) -> Result<String, String> {
            Err("interpret() should not be called when system prompt path is wired".to_string())
        }

        async fn interpret_with_system(
            &self,
            system_prompt: Option<&str>,
            _prompt: &str,
        ) -> Result<String, String> {
            self.seen_system_prompts.lock().unwrap().push(
                system_prompt
                    .map(str::to_string)
                    .unwrap_or_else(|| "<none>".to_string()),
            );
            Ok(r#"{
                "answer_type": "DirectAnswer",
                "slots": [],
                "correction_target": null,
                "correction_value": null,
                "clarification_text": null,
                "rejection_reason": null,
                "confidence": 0.9,
                "requires_replan": false,
                "reasoning": "ok"
            }"#
            .to_string())
        }
    }

    #[test]
    fn test_extract_confidence() {
        assert_eq!(extract_confidence("confidence: 0.8"), Some(0.8));
        assert_eq!(extract_confidence("Confidence = 0.75"), Some(0.75));
        assert_eq!(extract_confidence("confidence is 0.9"), Some(0.9));
        assert_eq!(extract_confidence("no confidence here"), None);
    }

    struct DummyPromptStore;

    #[async_trait::async_trait]
    impl PromptStore for DummyPromptStore {
        async fn get_prompt(&self, name: &str, version: &str) -> anyhow::Result<Prompt> {
            let content = if name
                == crate::magician_v2::prompts::constants::names::ANSWER_INTERPRETATION_SYSTEM
            {
                "System prompt\n{identity_section}".to_string()
            } else {
                "test prompt with {question} and {answer} and {context}".to_string()
            };
            Ok(Prompt::new(
                name.to_string(),
                version.to_string(),
                content,
                PromptCategory::General,
                "Test prompt for answer interpretation".to_string(),
                "test".to_string(),
            ))
        }

        async fn list_versions(&self, _name: &str) -> anyhow::Result<Vec<String>> {
            Ok(vec!["v1.0.0".to_string()])
        }

        async fn list_prompt_names(&self) -> anyhow::Result<Vec<String>> {
            Ok(vec!["answer_interpretation".to_string()])
        }

        async fn save_prompt(&self, _prompt: &Prompt) -> anyhow::Result<()> {
            Ok(())
        }

        async fn prompt_exists(&self, _name: &str, _version: &str) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn latest_version(&self, _name: &str) -> anyhow::Result<String> {
            Ok("v1.0.0".to_string())
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

    #[test]
    fn test_answer_type_detection() {
        let prompt_store: Arc<dyn PromptStore> = Arc::new(DummyPromptStore);
        let prompt_manager = Arc::new(PromptManager::new(prompt_store));
        let interpreter = AnswerInterpreter::new(MockLLM, prompt_manager);

        let correction = interpreter
            .parse_text_response("This is a correction, use WhatsApp instead")
            .unwrap();
        assert_eq!(correction.answer_type, AnswerType::DirectAnswer);
        assert!(correction.confidence <= 0.4);
        assert!(correction
            .clarification_text
            .as_deref()
            .unwrap()
            .contains("WhatsApp"));

        let rejection = interpreter
            .parse_text_response("I don't want to specify that now")
            .unwrap();
        assert_eq!(rejection.answer_type, AnswerType::DirectAnswer);
        assert!(rejection.correction_target.is_none());
        assert!(rejection
            .clarification_text
            .as_deref()
            .unwrap()
            .contains("Could you rephrase"));

        let direct = interpreter
            .parse_text_response("The answer is simple and direct")
            .unwrap();
        assert_eq!(direct.answer_type, AnswerType::DirectAnswer);
        assert!(direct.confidence <= 0.4);
        assert!(direct.reasoning.contains("LLM response was not valid JSON"));
    }

    #[tokio::test]
    async fn interpret_answer_with_identity_uses_system_prompt_path() {
        let (llm, seen_system_prompts) = SystemOnlyLLM::new();
        let prompt_store: Arc<dyn PromptStore> = Arc::new(DummyPromptStore);
        let prompt_manager = Arc::new(PromptManager::new(prompt_store));
        let interpreter = AnswerInterpreter::new(llm, prompt_manager);
        let identity = PromptIdentityContext {
            agent_kind: Some(PromptAgentKind::User),
            base_persona: Some("Careful".to_string()),
            source_agent_id: Some("agent-identity-test".to_string()),
            source_agent_name: Some("Assistant".to_string()),
            source_agent_aliases: Vec::new(),
            source_agent_persona: None,
            autonomous_controls: None,
        };

        let interpreted = interpreter
            .interpret_answer_with_identity(
                "What service should I use?",
                "Use checkout-service",
                Some("Clarification context"),
                Some(&identity),
            )
            .await
            .expect("interpreter should use interpret_with_system");
        assert_eq!(interpreted.answer_type, AnswerType::DirectAnswer);

        let prompts = seen_system_prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(
            prompts[0].contains("AGENT IDENTITY CONTEXT"),
            "system prompt should include identity section"
        );
        assert!(
            prompts[0].contains("Source agent id: agent-identity-test"),
            "system prompt should include source agent id"
        );
    }
}
