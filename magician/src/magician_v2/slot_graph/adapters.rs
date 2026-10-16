use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;

use super::{
    extraction::{LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService},
    rewriter::{RewriteModel, RewriteModelRequest, RewriteModelResponse},
};
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};

/// Deterministic LLM adapter used when no external extraction model is wired.
pub struct DeterministicExtractionLlm;

#[async_trait]
impl LlmService for DeterministicExtractionLlm {
    async fn call_function(
        &self,
        request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        let message = extract_section(&request.user_prompt, "User request:");

        let response = json!({
            "slots": [{
                "slot_type": "action",
                "value": {
                    "summary": message,
                },
                "confidence": 0.82,
                "rationale": "Heuristic extraction placeholder produced by deterministic adapter."
            }]
        });

        Ok(LlmFunctionCallResponse {
            raw_arguments: response.to_string(),
            telemetry: None,
        })
    }
}

/// Live extraction adapter backed by the operation-aware LLM router.
pub struct MultiLlmExtractionAdapter {
    service: Arc<OperationLlmRouter>,
}

impl MultiLlmExtractionAdapter {
    pub fn new(service: Arc<OperationLlmRouter>) -> Self {
        Self { service }
    }

    fn compose_prompt(request: &LlmFunctionCallRequest) -> String {
        format!(
            "{system}\n\nYou are provided with a JSON function schema. Produce a JSON arguments \
             payload that strictly conforms to it. Do not include any prose or code fences.\n\n\
             Function schema:\n{schema}\n\n=== User Input ===\n{user}\n\nRespond with the JSON \
             arguments only.",
            system = request.system_prompt.trim(),
            schema = request.function_schema.trim(),
            user = request.user_prompt.trim()
        )
    }
}

#[async_trait]
impl LlmService for MultiLlmExtractionAdapter {
    async fn call_function(
        &self,
        request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        let prompt = Self::compose_prompt(&request);
        let llm_response = self
            .service
            .generate_for_operation(&LLMOperation::SlotExtraction, &prompt)
            .await
            .with_context(|| "slot extraction call via operation LLM router failed")?;

        Ok(LlmFunctionCallResponse {
            raw_arguments: llm_response.content,
            telemetry: llm_response.telemetry,
        })
    }

    async fn call_function_scoped(
        &self,
        scope: magicllm::LlmScope,
        request: LlmFunctionCallRequest,
    ) -> Result<LlmFunctionCallResponse> {
        let prompt = Self::compose_prompt(&request);
        let llm_response = self
            .service
            .with_scope_context(Some(scope))
            .generate_for_operation(&LLMOperation::SlotExtraction, &prompt)
            .await
            .with_context(|| "scoped slot extraction call via operation LLM router failed")?;

        Ok(LlmFunctionCallResponse {
            raw_arguments: llm_response.content,
            telemetry: llm_response.telemetry,
        })
    }

    fn provider_name(&self) -> String {
        self.service
            .get_config_for_operation(&LLMOperation::SlotExtraction)
            .map(|config| config.provider.to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

/// Deterministic rewrite adapter that mirrors the user's request into a ClarifiedTask.
pub struct DeterministicRewriteModel;

#[async_trait]
impl RewriteModel for DeterministicRewriteModel {
    async fn generate(&self, request: RewriteModelRequest) -> Result<RewriteModelResponse> {
        let original = extract_section(&request.prompt, "Original user request:");
        let slot_summary = extract_section(&request.prompt, "Slot graph summary:");
        let unresolved =
            extract_section(&request.prompt, "Unresolved or missing slots to address:");

        let clarified = if original.is_empty() {
            "Plan the next steps.".to_string()
        } else {
            format!("Plan to accomplish: {}", original)
        };

        let mut objectives = Vec::new();
        if !slot_summary.is_empty() {
            objectives.push(format!("Leverage slot context: {}", slot_summary));
        }
        objectives.push("Produce a step-by-step execution outline.".to_string());

        let mut open_questions = Vec::new();
        if !unresolved.is_empty() {
            open_questions.push(format!("Clarify: {}", unresolved));
        }

        let payload = json!({
            "clarified_task": clarified,
            "constraints": [],
            "objectives": objectives,
            "resources": [],
            "open_questions": open_questions,
            "confidence": 0.76
        });

        Ok(RewriteModelResponse {
            completion: payload.to_string(),
            telemetry: None,
        })
    }
}

/// Rewrite adapter that calls the configured operation LLM router for live completions.
pub struct MultiLlmRewriteModel {
    llm_service: Arc<OperationLlmRouter>,
    operation: LLMOperation,
}

impl MultiLlmRewriteModel {
    pub fn new(llm_service: Arc<OperationLlmRouter>, operation: LLMOperation) -> Self {
        Self {
            llm_service,
            operation,
        }
    }
}

#[async_trait]
impl RewriteModel for MultiLlmRewriteModel {
    async fn generate(&self, request: RewriteModelRequest) -> Result<RewriteModelResponse> {
        let llm_response = self
            .llm_service
            .generate_for_operation(&self.operation, &request.prompt)
            .await
            .with_context(|| {
                format!(
                    "multi-LLM rewrite call failed for operation '{}'",
                    self.operation.as_str()
                )
            })?;

        Ok(RewriteModelResponse {
            completion: llm_response.content,
            telemetry: llm_response.telemetry,
        })
    }

    async fn generate_scoped(
        &self,
        scope: magicllm::LlmScope,
        request: RewriteModelRequest,
    ) -> Result<RewriteModelResponse> {
        let llm_response = self
            .llm_service
            .with_scope_context(Some(scope))
            .generate_for_operation(&self.operation, &request.prompt)
            .await
            .with_context(|| {
                format!(
                    "scoped multi-LLM rewrite call failed for operation '{}'",
                    self.operation.as_str()
                )
            })?;

        Ok(RewriteModelResponse {
            completion: llm_response.content,
            telemetry: llm_response.telemetry,
        })
    }
}

fn extract_section(prompt: &str, header: &str) -> String {
    let mut lines = prompt.lines();
    while let Some(line) = lines.next() {
        if line.trim() == header {
            return lines
                .by_ref()
                .take_while(|l| !l.trim().is_empty())
                .map(|l| l.trim().trim_matches('"'))
                .collect::<Vec<_>>()
                .join(" ");
        }
    }

    String::new()
}
